# Security policy

Report suspected vulnerabilities privately through GitHub's private vulnerability reporting for this
repository (**Security** tab, then **Report a vulnerability**). Please do not open a public issue.

In scope: a launcher that lets a job read or write outside what its spec mounted, leave state for the
next run, or outlive its environment; a capability marked `yes` in the matrix that its test does not
prove; a secret value appearing in a log, file or environment.
