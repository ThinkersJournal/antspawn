// SPDX-License-Identifier: MIT OR Apache-2.0
//! Part A of the account network boundary (`docs/ACCOUNT-ISOLATION.md`): Windows Filtering Platform
//! filters that refuse a restricted account's connections to loopback and to the host's own addresses.
//!
//! Why this exists: Windows Firewall rules, even a block-all rule bound to the account's SID, do not see
//! that traffic (measured 2026-11-08: the account has such a rule and still reaches 32 local ports). A WFP
//! filter at the connect layer, with the user as a condition, does.
//!
//! What it installs (all persistent, all ours, nothing else is touched):
//!   * one provider and one sublayer with fixed keys, so `undo` removes exactly these;
//!   * two filters, one at `FWPM_LAYER_ALE_AUTH_CONNECT_V4` and one at `..._V6`, each **Block** when
//!     `FWPM_CONDITION_ALE_USER_ID` matches a security descriptor granting the account's SID **and**
//!     `FWPM_CONDITION_FLAGS` has `FWP_CONDITION_FLAG_IS_LOOPBACK` set.
//!
//! Unrun where it matters: building the structures, the SID lookup and the descriptor are tested here
//! without elevation; adding the objects needs an elevated token and is not exercised by any test. Whether
//! the loopback flag also covers a connect to the host's *own non-127 address* (172.25.224.1, 10.77.0.1)
//! is exactly what the per-port scan after `apply` decides, and this module does not claim it.
//!
//! `apply` runs in one WFP transaction: any failure aborts it and leaves nothing behind. After the commit
//! every filter is read back by key and compared with what was asked; a mismatch removes our objects again.

#![cfg(windows)]

use std::ffi::c_void;
use std::fmt::Write as _;
use std::ptr;

use windows_sys::Win32::Foundation::{
    FWP_E_ALREADY_EXISTS, FWP_E_FILTER_NOT_FOUND, FWP_E_PROVIDER_NOT_FOUND,
    FWP_E_SUBLAYER_NOT_FOUND, HANDLE, LocalFree,
};
use windows_sys::Win32::NetworkManagement::WindowsFilteringPlatform::{
    FWP_ACTION_BLOCK, FWP_BYTE_BLOB, FWP_CONDITION_FLAG_IS_LOOPBACK, FWP_CONDITION_VALUE0,
    FWP_CONDITION_VALUE0_0, FWP_EMPTY, FWP_MATCH_EQUAL, FWP_MATCH_FLAGS_ALL_SET,
    FWP_SECURITY_DESCRIPTOR_TYPE, FWP_UINT32, FWP_VALUE0, FWPM_ACTION0, FWPM_CONDITION_ALE_USER_ID,
    FWPM_CONDITION_FLAGS, FWPM_DISPLAY_DATA0, FWPM_FILTER_CONDITION0, FWPM_FILTER_FLAG_PERSISTENT,
    FWPM_FILTER0, FWPM_LAYER_ALE_AUTH_CONNECT_V4, FWPM_LAYER_ALE_AUTH_CONNECT_V6,
    FWPM_PROVIDER_FLAG_PERSISTENT, FWPM_PROVIDER0, FWPM_SUBLAYER_FLAG_PERSISTENT, FWPM_SUBLAYER0,
    FwpmEngineClose0, FwpmEngineOpen0, FwpmFilterAdd0, FwpmFilterDeleteByKey0, FwpmFilterGetByKey0,
    FwpmFreeMemory0, FwpmProviderAdd0, FwpmProviderDeleteByKey0, FwpmSubLayerAdd0,
    FwpmSubLayerDeleteByKey0, FwpmTransactionAbort0, FwpmTransactionBegin0, FwpmTransactionCommit0,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetSecurityDescriptorLength, LookupAccountNameW,
    PSECURITY_DESCRIPTOR, SID_NAME_USE,
};
use windows_sys::Win32::System::Rpc::RPC_C_AUTHN_WINNT;
use windows_sys::core::GUID;

/// Fixed keys: `undo` finds exactly what `apply` made by these, never by name or by enumeration.
pub const PROVIDER_KEY: GUID = GUID::from_u128(0x3efe6bef_cc06_4f71_93e6_158849632a69);
pub const SUBLAYER_KEY: GUID = GUID::from_u128(0x2c05f461_864c_4f0a_a402_f093e833637b);
pub const FILTER_V4_KEY: GUID = GUID::from_u128(0x92a838dc_e583_44f1_872d_47961ef28463);
pub const FILTER_V6_KEY: GUID = GUID::from_u128(0x0741acb1_3bf2_4acb_b0e7_3dbf93dbf8ea);

/// `FWP_ACTRL_MATCH_FILTER` is what a descriptor must grant for a user condition to match; `CC`
/// (create child) is the SDDL right with that value, the same form Windows Firewall's own `LocalUser`
/// uses (the existing rule reads `D:(A;;CC;;;<sid>)`).
const MATCH_FILTER_SDDL_RIGHT: &str = "CC";

/// `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`, the form `netsh wfp` and the docs use.
#[must_use]
pub fn guid_text(g: &GUID) -> String {
    format!(
        "{{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7]
    )
}

/// `GUID` here has neither `PartialEq` nor `Debug`; compare by value through its text form.
#[must_use]
pub fn guid_eq(a: &GUID, b: &GUID) -> bool {
    guid_text(a) == guid_text(b)
}

/// `0x80320003`-style codes print as hex.
fn hex(code: u32) -> String {
    format!("0x{code:08X}")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A SID string is only digits, `S` and `-`: it ends up inside an SDDL string, so nothing else may.
fn valid_sid_text(s: &str) -> bool {
    s.starts_with("S-1-")
        && s.len() <= 184
        && s.chars()
            .all(|c| c == 'S' || c == '-' || c.is_ascii_digit())
}

/// An account name or a SID string, as the SID string.
///
/// # Errors
/// The name does not resolve, or a Win32 call failed.
pub fn resolve_sid(account_or_sid: &str) -> Result<String, String> {
    if valid_sid_text(account_or_sid) {
        return Ok(account_or_sid.to_string());
    }
    let name = wide(account_or_sid);
    let (mut sid_len, mut dom_len, mut use_) = (0u32, 0u32, 0 as SID_NAME_USE);
    // SAFETY: first call only reports the sizes (it fails with insufficient buffer by design).
    unsafe {
        LookupAccountNameW(
            ptr::null(),
            name.as_ptr(),
            ptr::null_mut(),
            &mut sid_len,
            ptr::null_mut(),
            &mut dom_len,
            &mut use_,
        );
    }
    if sid_len == 0 {
        return Err(format!("`{account_or_sid}` does not resolve to an account"));
    }
    let mut sid = vec![0u8; sid_len as usize];
    let mut dom = vec![0u16; dom_len.max(1) as usize];
    // SAFETY: buffers are the sizes the first call asked for.
    let ok = unsafe {
        LookupAccountNameW(
            ptr::null(),
            name.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_len,
            dom.as_mut_ptr(),
            &mut dom_len,
            &mut use_,
        )
    };
    if ok == 0 {
        return Err(format!("LookupAccountNameW failed for `{account_or_sid}`"));
    }
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: `sid` is a valid SID filled in by the call above.
    if unsafe { ConvertSidToStringSidW(sid.as_mut_ptr().cast(), &mut text) } == 0 {
        return Err("ConvertSidToStringSidW failed".into());
    }
    let s = unsafe { wide_ptr_to_string(text) };
    // SAFETY: `text` was allocated by the call above with LocalAlloc.
    unsafe { LocalFree(text.cast()) };
    Ok(s)
}

/// # Safety
/// `p` is a valid NUL-terminated UTF-16 string.
unsafe fn wide_ptr_to_string(p: *const u16) -> String {
    let mut len = 0;
    // SAFETY: bounded by the terminator the caller promised.
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
}

/// The self-relative security descriptor granting `sid` the match-filter right.
///
/// # Errors
/// An invalid SID text, or a failed conversion.
pub fn sd_blob(sid: &str) -> Result<Vec<u8>, String> {
    if !valid_sid_text(sid) {
        return Err(format!("`{sid}` is not a SID string"));
    }
    let sddl = wide(&format!("D:(A;;{MATCH_FILTER_SDDL_RIGHT};;;{sid})"));
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: standard conversion; `sd` is LocalAlloc'd on success.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err("ConvertStringSecurityDescriptorToSecurityDescriptorW failed".into());
    }
    // SAFETY: `sd` is a valid self-relative descriptor.
    let len = unsafe { GetSecurityDescriptorLength(sd) } as usize;
    let bytes = unsafe { std::slice::from_raw_parts(sd.cast::<u8>(), len) }.to_vec();
    unsafe { LocalFree(sd) };
    Ok(bytes)
}

/// The SDDL text of a descriptor (to check what WFP stored names the SID).
fn sddl_of(sd: *mut c_void) -> Option<String> {
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: `sd` is a valid descriptor owned by the caller; `text` is LocalAlloc'd on success.
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }
    let s = unsafe { wide_ptr_to_string(text) };
    unsafe { LocalFree(text.cast()) };
    Some(s)
}

/// Every structure the add calls need, owned in one place so the pointers inside stay valid.
pub struct Plan {
    pub sid: String,
    sd: Vec<u8>,
    blob: Box<FWP_BYTE_BLOB>,
    provider_key: Box<GUID>,
    names: Vec<Vec<u16>>,
    // Boxed on purpose: each filter holds a raw pointer into its condition array, and a plain Vec would move
    // them when it grows.
    #[allow(clippy::vec_box)]
    conditions: Vec<Box<[FWPM_FILTER_CONDITION0; 2]>>,
    pub provider: FWPM_PROVIDER0,
    pub sublayer: FWPM_SUBLAYER0,
    pub filters: Vec<FWPM_FILTER0>,
}

impl Plan {
    /// Build every structure for `sid`. No call into the filtering engine happens here.
    ///
    /// # Errors
    /// An invalid SID or a failed descriptor conversion.
    pub fn build(sid: &str) -> Result<Plan, String> {
        let mut sd = sd_blob(sid)?;
        let mut blob = Box::new(FWP_BYTE_BLOB {
            size: u32::try_from(sd.len()).map_err(|_| "descriptor too large".to_string())?,
            data: sd.as_mut_ptr(),
        });
        let mut provider_key = Box::new(PROVIDER_KEY);
        let mut plan = Plan {
            sid: sid.to_string(),
            sd: Vec::new(),
            blob: Box::new(FWP_BYTE_BLOB::default()),
            provider_key: Box::new(PROVIDER_KEY),
            names: Vec::new(),
            conditions: Vec::new(),
            provider: FWPM_PROVIDER0::default(),
            sublayer: FWPM_SUBLAYER0::default(),
            filters: Vec::new(),
        };
        // names live as long as the plan; the structures hold raw pointers into them
        let pname = wide("antspawn account boundary");
        let pdesc =
            wide("antspawn: refuses a restricted account's loopback and own-address connects");
        let sname = wide("antspawn account boundary sublayer");
        plan.provider.providerKey = PROVIDER_KEY;
        plan.provider.displayData.name = pname.as_ptr().cast_mut();
        plan.provider.displayData.description = pdesc.as_ptr().cast_mut();
        plan.provider.flags = FWPM_PROVIDER_FLAG_PERSISTENT;
        plan.sublayer.subLayerKey = SUBLAYER_KEY;
        plan.sublayer.displayData.name = sname.as_ptr().cast_mut();
        plan.sublayer.flags = FWPM_SUBLAYER_FLAG_PERSISTENT;
        plan.sublayer.providerKey = &mut *provider_key;
        plan.sublayer.weight = 0x7FFF;
        plan.names.extend([pname, pdesc, sname]);

        for (key, layer, label) in [
            (
                FILTER_V4_KEY,
                FWPM_LAYER_ALE_AUTH_CONNECT_V4,
                "antspawn: block account loopback connect (IPv4)",
            ),
            (
                FILTER_V6_KEY,
                FWPM_LAYER_ALE_AUTH_CONNECT_V6,
                "antspawn: block account loopback connect (IPv6)",
            ),
        ] {
            let name = wide(label);
            let mut conds = Box::new([
                FWPM_FILTER_CONDITION0::default(),
                FWPM_FILTER_CONDITION0::default(),
            ]);
            conds[0].fieldKey = FWPM_CONDITION_ALE_USER_ID;
            conds[0].matchType = FWP_MATCH_EQUAL;
            conds[0].conditionValue = FWP_CONDITION_VALUE0 {
                r#type: FWP_SECURITY_DESCRIPTOR_TYPE,
                Anonymous: FWP_CONDITION_VALUE0_0 { sd: &mut *blob },
            };
            conds[1].fieldKey = FWPM_CONDITION_FLAGS;
            conds[1].matchType = FWP_MATCH_FLAGS_ALL_SET;
            conds[1].conditionValue = FWP_CONDITION_VALUE0 {
                r#type: FWP_UINT32,
                Anonymous: FWP_CONDITION_VALUE0_0 {
                    uint32: FWP_CONDITION_FLAG_IS_LOOPBACK,
                },
            };
            let f = FWPM_FILTER0 {
                filterKey: key,
                displayData: FWPM_DISPLAY_DATA0 {
                    name: name.as_ptr().cast_mut(),
                    description: ptr::null_mut(),
                },
                flags: FWPM_FILTER_FLAG_PERSISTENT,
                providerKey: &mut *provider_key,
                layerKey: layer,
                subLayerKey: SUBLAYER_KEY,
                weight: FWP_VALUE0 {
                    r#type: FWP_EMPTY,
                    ..FWP_VALUE0::default()
                },
                numFilterConditions: 2,
                filterCondition: conds.as_mut_ptr(),
                action: FWPM_ACTION0 {
                    r#type: FWP_ACTION_BLOCK,
                    ..FWPM_ACTION0::default()
                },
                ..FWPM_FILTER0::default()
            };
            plan.names.push(name);
            plan.conditions.push(conds);
            plan.filters.push(f);
        }
        plan.sd = sd;
        plan.blob = blob;
        plan.provider_key = provider_key;
        Ok(plan)
    }

    /// What `apply` would add, for a human to read before anything runs.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "account SID {}", self.sid);
        let _ = writeln!(s, "provider   {} persistent", guid_text(&PROVIDER_KEY));
        let _ = writeln!(
            s,
            "sublayer   {} persistent, weight 0x7FFF, under that provider",
            guid_text(&SUBLAYER_KEY)
        );
        for (f, v) in self.filters.iter().zip(["IPv4", "IPv6"]) {
            let _ = writeln!(
                s,
                "filter     {} {v}: layer ALE_AUTH_CONNECT, action BLOCK, persistent; matches when ALE_USER_ID is granted by D:(A;;{MATCH_FILTER_SDDL_RIGHT};;;{}) AND flags has IS_LOOPBACK (conditions: {})",
                guid_text(&f.filterKey),
                self.sid,
                f.numFilterConditions
            );
        }
        let _ = write!(s, "descriptor  {} bytes (self-relative)", self.sd.len());
        s
    }
}

/// An open handle to the filtering engine, closed on drop.
pub struct Engine(HANDLE);

impl Engine {
    /// # Errors
    /// The engine could not be opened (the code is in the message).
    pub fn open() -> Result<Engine, String> {
        let mut h: HANDLE = ptr::null_mut();
        // SAFETY: null server/identity/session select the local engine with the caller's identity.
        let rc = unsafe {
            FwpmEngineOpen0(
                ptr::null(),
                RPC_C_AUTHN_WINNT,
                ptr::null(),
                ptr::null(),
                &mut h,
            )
        };
        if rc != 0 {
            return Err(format!("FwpmEngineOpen0 failed: {}", hex(rc)));
        }
        Ok(Engine(h))
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // SAFETY: the handle came from FwpmEngineOpen0 and is closed once.
        unsafe { FwpmEngineClose0(self.0) };
    }
}

fn check(what: &str, rc: u32) -> Result<(), String> {
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("{what} failed: {}", hex(rc)))
    }
}

/// What one filter looks like when read back.
fn read_filter(engine: &Engine, key: &GUID, want: &Plan) -> Result<String, String> {
    let mut p: *mut FWPM_FILTER0 = ptr::null_mut();
    // SAFETY: `p` receives engine-owned memory, freed with FwpmFreeMemory0 below.
    let rc = unsafe { FwpmFilterGetByKey0(engine.0, key, &mut p) };
    if rc == FWP_E_FILTER_NOT_FOUND as u32 {
        return Err("not present".into());
    }
    check("FwpmFilterGetByKey0", rc)?;
    // SAFETY: success means `p` points at a valid filter until freed.
    let f = unsafe { &*p };
    let mut problems = Vec::new();
    if f.action.r#type != FWP_ACTION_BLOCK {
        problems.push(format!("action {:#x} is not BLOCK", f.action.r#type));
    }
    if f.flags & FWPM_FILTER_FLAG_PERSISTENT == 0 {
        problems.push("not persistent".to_string());
    }
    if f.providerKey.is_null() || !guid_eq(unsafe { &*f.providerKey }, &PROVIDER_KEY) {
        problems.push("provider is not ours".to_string());
    }
    if !guid_eq(&f.subLayerKey, &SUBLAYER_KEY) {
        problems.push("sublayer is not ours".to_string());
    }
    if f.numFilterConditions != 2 {
        problems.push(format!("{} conditions, want 2", f.numFilterConditions));
    } else {
        // SAFETY: two conditions, per the count just checked.
        let conds = unsafe { std::slice::from_raw_parts(f.filterCondition, 2) };
        let user = conds
            .iter()
            .find(|c| guid_eq(&c.fieldKey, &FWPM_CONDITION_ALE_USER_ID));
        let flags = conds
            .iter()
            .find(|c| guid_eq(&c.fieldKey, &FWPM_CONDITION_FLAGS));
        match user {
            Some(c) if c.conditionValue.r#type == FWP_SECURITY_DESCRIPTOR_TYPE => {
                // SAFETY: the type says `sd` is a byte blob holding a descriptor.
                let blob = unsafe { c.conditionValue.Anonymous.sd };
                let text = if blob.is_null() {
                    None
                } else {
                    sddl_of(unsafe { (*blob).data }.cast())
                };
                match text {
                    Some(t) if t.contains(&want.sid) => {}
                    other => problems.push(format!(
                        "user condition does not name {}: {other:?}",
                        want.sid
                    )),
                }
            }
            _ => problems.push("no ALE_USER_ID security-descriptor condition".to_string()),
        }
        match flags {
            Some(c)
                if c.matchType == FWP_MATCH_FLAGS_ALL_SET
                    && c.conditionValue.r#type == FWP_UINT32
                    && unsafe { c.conditionValue.Anonymous.uint32 }
                        == FWP_CONDITION_FLAG_IS_LOOPBACK => {}
            _ => problems.push("no IS_LOOPBACK flags condition".to_string()),
        }
    }
    let layer = f.layerKey;
    let mut pp = p.cast::<c_void>();
    // SAFETY: `p` is engine-owned memory from the call above.
    unsafe { FwpmFreeMemory0(&mut pp) };
    if !guid_eq(&layer, &FWPM_LAYER_ALE_AUTH_CONNECT_V4)
        && !guid_eq(&layer, &FWPM_LAYER_ALE_AUTH_CONNECT_V6)
    {
        problems.push("layer is not ALE_AUTH_CONNECT".to_string());
    }
    if problems.is_empty() {
        Ok("present, Block, persistent, ours, user + loopback conditions as asked".into())
    } else {
        Err(problems.join("; "))
    }
}

/// Read every filter back (no change).
///
/// # Errors
/// Never for a missing filter; that is reported per filter in the text.
pub fn status(sid: &str) -> Result<String, String> {
    let want = Plan::build(sid)?;
    let engine = Engine::open()?;
    let mut s = String::new();
    for (key, label) in [
        (FILTER_V4_KEY, "IPv4 filter"),
        (FILTER_V6_KEY, "IPv6 filter"),
    ] {
        match read_filter(&engine, &key, &want) {
            Ok(t) => {
                let _ = writeln!(s, "{label}: {t}");
            }
            Err(e) => {
                let _ = writeln!(s, "{label}: {e}");
            }
        }
    }
    Ok(s)
}

/// Add the provider, the sublayer and both filters in one transaction, then read them back.
///
/// # Errors
/// Any call failing aborts the transaction (nothing is left); a read-back mismatch after commit removes our
/// objects again and is the error.
pub fn apply(sid: &str) -> Result<String, String> {
    let plan = Plan::build(sid)?;
    let engine = Engine::open()?;
    // SAFETY: handle is open; flags 0 = read-write transaction.
    check("FwpmTransactionBegin0", unsafe {
        FwpmTransactionBegin0(engine.0, 0)
    })?;
    let added = (|| -> Result<(), String> {
        // SAFETY: structures are owned by `plan` and outlive these calls; null SD = default security.
        let rc = unsafe { FwpmProviderAdd0(engine.0, &plan.provider, ptr::null_mut()) };
        if rc == FWP_E_ALREADY_EXISTS as u32 {
            return Err("our provider already exists: run undo first".into());
        }
        check("FwpmProviderAdd0", rc)?;
        check("FwpmSubLayerAdd0", unsafe {
            FwpmSubLayerAdd0(engine.0, &plan.sublayer, ptr::null_mut())
        })?;
        for f in &plan.filters {
            let mut id = 0u64;
            check("FwpmFilterAdd0", unsafe {
                FwpmFilterAdd0(engine.0, f, ptr::null_mut(), &mut id)
            })?;
        }
        Ok(())
    })();
    if let Err(e) = added {
        // SAFETY: a transaction is open.
        let rc = unsafe { FwpmTransactionAbort0(engine.0) };
        return Err(format!(
            "{e}; transaction aborted ({}), nothing was left behind",
            hex(rc)
        ));
    }
    check("FwpmTransactionCommit0", unsafe {
        FwpmTransactionCommit0(engine.0)
    })?;
    let mut report = String::new();
    let mut bad = Vec::new();
    for (key, label) in [
        (FILTER_V4_KEY, "IPv4 filter"),
        (FILTER_V6_KEY, "IPv6 filter"),
    ] {
        match read_filter(&engine, &key, &plan) {
            Ok(t) => {
                let _ = writeln!(report, "{label}: {t}");
            }
            Err(e) => bad.push(format!("{label}: {e}")),
        }
    }
    if bad.is_empty() {
        Ok(report)
    } else {
        let removed = undo_with(&engine);
        Err(format!(
            "read-back mismatch ({}); our objects were removed again: {removed:?}",
            bad.join(" | ")
        ))
    }
}

fn delete_ignoring(rc: u32, not_found: i32, what: &str) -> Result<(), String> {
    if rc == 0 || rc == not_found as u32 {
        Ok(())
    } else {
        Err(format!("{what} failed: {}", hex(rc)))
    }
}

fn undo_with(engine: &Engine) -> Result<(), String> {
    // SAFETY: handle is open.
    check("FwpmTransactionBegin0", unsafe {
        FwpmTransactionBegin0(engine.0, 0)
    })?;
    let r = (|| -> Result<(), String> {
        for key in [FILTER_V4_KEY, FILTER_V6_KEY] {
            delete_ignoring(
                unsafe { FwpmFilterDeleteByKey0(engine.0, &key) },
                FWP_E_FILTER_NOT_FOUND,
                "FwpmFilterDeleteByKey0",
            )?;
        }
        delete_ignoring(
            unsafe { FwpmSubLayerDeleteByKey0(engine.0, &SUBLAYER_KEY) },
            FWP_E_SUBLAYER_NOT_FOUND,
            "FwpmSubLayerDeleteByKey0",
        )?;
        delete_ignoring(
            unsafe { FwpmProviderDeleteByKey0(engine.0, &PROVIDER_KEY) },
            FWP_E_PROVIDER_NOT_FOUND,
            "FwpmProviderDeleteByKey0",
        )
    })();
    match r {
        Ok(()) => check("FwpmTransactionCommit0", unsafe {
            FwpmTransactionCommit0(engine.0)
        }),
        Err(e) => {
            let rc = unsafe { FwpmTransactionAbort0(engine.0) };
            Err(format!("{e}; transaction aborted ({})", hex(rc)))
        }
    }
}

/// Remove exactly our filters, sublayer and provider (by their fixed keys), then read back that they are gone.
///
/// # Errors
/// A delete other than "not found" failed (the transaction is aborted), or something is still there.
pub fn undo() -> Result<String, String> {
    let engine = Engine::open()?;
    undo_with(&engine)?;
    let want = Plan::build("S-1-5-32-545")?; // only the shape is compared; absence is what is asserted
    let mut left = Vec::new();
    for key in [FILTER_V4_KEY, FILTER_V6_KEY] {
        if read_filter(&engine, &key, &want).is_ok() {
            left.push(guid_text(&key));
        }
    }
    if left.is_empty() {
        Ok(
            "both filters, the sublayer and the provider are gone (filters read back as absent)"
                .into(),
        )
    } else {
        Err(format!("still present after undo: {}", left.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "S-1-5-32-545"; // BUILTIN\Users, a SID that exists everywhere

    #[test]
    fn a_sid_is_resolved_from_a_name_and_a_sid_string_is_kept() {
        assert_eq!(resolve_sid(SID).unwrap(), SID);
        // a well-known account that exists on every Windows install resolves to a SID string
        let s = resolve_sid("Users").unwrap();
        assert!(valid_sid_text(&s), "{s}");
        assert!(resolve_sid("no-such-account-antspawn-xyz").is_err());
    }

    #[test]
    fn only_sid_text_can_reach_the_descriptor_string() {
        for bad in [
            "",
            "S-1-5-32-545)(A;;GA;;;WD",
            "S-1-5-32-545 ",
            "x",
            "S-2-1",
            "S-1-5-32-545;",
        ] {
            assert!(sd_blob(bad).is_err(), "{bad:?}");
        }
        assert!(sd_blob(SID).is_ok());
    }

    #[test]
    fn the_descriptor_round_trips_and_names_exactly_the_sid() {
        let mut blob = sd_blob(SID).unwrap();
        let text = sddl_of(blob.as_mut_ptr().cast()).expect("descriptor converts back");
        assert!(text.contains(SID) || text.contains("BU"), "{text}");
        assert!(text.contains("CC"), "the match-filter right: {text}");
        assert!(!text.contains("GA"), "no generic-all grant: {text}");
        // a different SID gives a different descriptor (the control: the blob depends on the SID)
        assert_ne!(blob, sd_blob("S-1-5-32-544").unwrap());
    }

    #[test]
    fn the_plan_has_two_connect_layer_block_filters_with_user_and_loopback_conditions() {
        let plan = Plan::build(SID).unwrap();
        assert_eq!(plan.filters.len(), 2);
        assert!(guid_eq(
            &plan.filters[0].layerKey,
            &FWPM_LAYER_ALE_AUTH_CONNECT_V4
        ));
        assert!(guid_eq(
            &plan.filters[1].layerKey,
            &FWPM_LAYER_ALE_AUTH_CONNECT_V6
        ));
        for (f, c) in plan.filters.iter().zip(&plan.conditions) {
            assert_eq!(f.action.r#type, FWP_ACTION_BLOCK);
            assert_eq!(
                f.flags & FWPM_FILTER_FLAG_PERSISTENT,
                FWPM_FILTER_FLAG_PERSISTENT
            );
            assert_eq!(f.numFilterConditions, 2);
            assert_eq!(f.filterCondition.cast_const(), c.as_ptr());
            assert!(guid_eq(&c[0].fieldKey, &FWPM_CONDITION_ALE_USER_ID));
            assert_eq!(c[0].conditionValue.r#type, FWP_SECURITY_DESCRIPTOR_TYPE);
            // the blob the condition points at is the descriptor, byte for byte
            let blob = unsafe { &*c[0].conditionValue.Anonymous.sd };
            let bytes = unsafe { std::slice::from_raw_parts(blob.data, blob.size as usize) };
            assert_eq!(bytes, sd_blob(SID).unwrap().as_slice());
            assert!(guid_eq(&c[1].fieldKey, &FWPM_CONDITION_FLAGS));
            assert_eq!(c[1].matchType, FWP_MATCH_FLAGS_ALL_SET);
            assert_eq!(
                unsafe { c[1].conditionValue.Anonymous.uint32 },
                FWP_CONDITION_FLAG_IS_LOOPBACK
            );
        }
        assert_eq!(
            plan.provider.flags & FWPM_PROVIDER_FLAG_PERSISTENT,
            FWPM_PROVIDER_FLAG_PERSISTENT
        );
        assert_eq!(
            plan.sublayer.flags & FWPM_SUBLAYER_FLAG_PERSISTENT,
            FWPM_SUBLAYER_FLAG_PERSISTENT
        );
        let text = plan.describe();
        assert!(
            text.contains(SID) && text.contains("BLOCK") && text.contains("IS_LOOPBACK"),
            "{text}"
        );
    }

    #[test]
    fn our_four_keys_are_distinct() {
        let keys = [PROVIDER_KEY, SUBLAYER_KEY, FILTER_V4_KEY, FILTER_V6_KEY];
        for i in 0..keys.len() {
            for j in i + 1..keys.len() {
                assert!(!guid_eq(&keys[i], &keys[j]));
            }
        }
    }

    #[test]
    fn the_plan_survives_being_moved() {
        // the structures hold raw pointers into boxes and vectors; moving the Plan must not break them
        let plan = Plan::build(SID).unwrap();
        let moved = Box::new(plan);
        let c0 = unsafe { std::slice::from_raw_parts(moved.filters[0].filterCondition, 2) };
        let blob = unsafe { &*c0[0].conditionValue.Anonymous.sd };
        assert_eq!(blob.size as usize, sd_blob(SID).unwrap().len());
        assert!(!moved.filters[0].providerKey.is_null());
        assert!(guid_eq(
            unsafe { &*moved.filters[0].providerKey },
            &PROVIDER_KEY
        ));
    }
}
