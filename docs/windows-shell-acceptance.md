# Windows shell acceptance

Use a disposable Windows guest and a synthetic profile. Record the Windows build, Git revision, SHA-256 of Setup or the portable archive, and the launch path for each scenario. Keep the process and window traces with the result. The checker reads normalized evidence and leaves all Windows settings untouched.

| Scenario | Start the trace before | End the scenario after |
| --- | --- | --- |
| `setup` | Opening Setup | Installation completes and first launch returns its expected result |
| `portable` | Opening the shipped portable entry point | The editor opens and the synthetic scene is available |
| `logon` | Signing in to the test account | The installed app starts and restores the synthetic scene |
| `rgb-recovery` | Interrupting the simulated RGB receiver | The receiver gets the latest scene after recovery |
| `upgrade` | Running Setup over the pinned earlier release | The updated app can read the preserved profile and local bindings |
| `uninstall` | Launching the installed uninstaller | The uninstall receipt confirms the expected owned artifacts were removed |
| `legacy-migration` | Starting the equivalent legacy import path | The migration receipt completes and the target files match the fixture |

Capture process start and stop events, with executable names, parent PIDs and session IDs. Use process identity `PID:start-filetime`, including when the same PID appears twice. Resolve the parent to the instance alive when the child started. Record the launcher or Setup as a product root, even if it is itself a shell. Register detached Task Scheduler launches and elevated helpers as separate product roots using the verified executable and the scenario's PID evidence. For an actual logon, collect process events from before sign-in and establish window coverage for the interactive desktop. A manually replayed command covers the launch path only. [Windows process start events](https://learn.microsoft.com/en-us/previous-versions/windows/desktop/krnlprov/win32-processstarttrace) · [PID lifetime](https://learn.microsoft.com/en-us/windows/win32/cimwin32prov/win32-process)

In each measured desktop, record visible console shows and windows already visible at the scenario boundary. A guest collector can use `SetWinEventHook` with a message loop and resolve the window owner with `GetWindowThreadProcessId`. Include both classic console windows and Windows Terminal windows. Keep unresolved visible console events in the evidence with a null process. Run a known short-lived shell and a visible console outside the product interval to establish that both collectors report events. Mark the matching control false if a subscription failed, events were lost, the desktop was uncovered, or the positive control failed. [WinEvent hooks](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwineventhook) · [Console events](https://learn.microsoft.com/en-us/previous-versions/windows/desktop/dnacc/console-accessibility)

Normalize one JSON file per scenario with `scenario`, `revision`, `artifact_sha256`, and `controls`. The `controls` keys are `process_start_stop`, `visible_console`, and `full_scenario`. Include `product_roots` and `harness_roots` as process identity lists. Each `processes` entry has `id`, `parent` (identity or null), and `image`. Each `windows` entry has `process` (identity or null), `class`, and boolean `visible`. Retain only the process and window data needed for the test. Harness shells are recognized by ancestry, including when a shell launches a product root. A shell under a product root still counts against the product.

```powershell
python scripts/shell_acceptance.py evidence.json --revision <commit> --artifact-sha256 <sha256>
```

Exit code `0` confirms the scenario on the expected artifact with complete controls. Exit code `1` reports a shell started by the product or a visible product console. Exit code `2` means evidence is invalid or incomplete, including an unattributed shell or visible console. Apply the same checker to the control release and to the combined Setup, runtime and portable candidate. Preserve the three component heads and the final candidate tree in the evidence.
