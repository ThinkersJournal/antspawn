kind: account pool sized to four accounts
---
CireSnave's ruling on board 420 (2026-11-12, "420: B.") fixes the pool at four accounts. `docs/ACCOUNT-POOL-SETUP.md` now gives the run and undo lines as `-Count 4` and the four-member `ANTSPAWN_POOL`, and `docs/SESSION-PLAN.md` records N = 4. The script already accepts 1-8, so no code changes. Not measured: a `-Count 4` dry run, any `-Apply`, and the pool test beyond two concurrent sessions (it exercises two of the four accounts, plus its two known-bad controls).
