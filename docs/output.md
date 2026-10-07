# Output and exit codes

`instantctl` writes command data to stdout and errors to stderr. On a terminal,
the default format is a table. When stdout is piped, the default is JSON. Set
`--format json`, `--format yaml`, or `--format table` to choose a format.

JSON errors contain a `kind` and a human-readable `message`. Successful
mutation output includes the outcome, the observed state, and whether a request
was attempted. A successful request with matching fresh readback is reported
as verified. If the request succeeds but readback does not establish the
requested state, the command reports an unverified result and exits with code
6. A matching observation does not erase a request failure.

| Exit code | Meaning |
| --- | --- |
| 0 | Command succeeded. |
| 1 | General error. |
| 2 | Configuration, usage, or confirmation is required. |
| 3 | Authentication error. |
| 4 | Resource not found, operation unsupported, or API result incomplete. |
| 5 | Client error or retry later. |
| 6 | A write was not verified by readback. |
| 141 | The stdout pipe closed while output was being written. |

Exit code 0 means the command's stated operation succeeded. It does not prove
that an unsupported or skipped operation occurred. Read the command result and
the relevant [capability notes](../CAPABILITIES.md), especially before relying
on a configuration change.
