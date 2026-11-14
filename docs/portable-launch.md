# Portable launch

Extract the complete Windows ZIP into a folder you can write to. The launchers use the extracted folder, regardless of the current working directory. They do not install anything, request administrator rights, register Windows startup tasks or start OpenRGB.

## First setup

1. Run `Open Prismestra.cmd`. This opens a software preview, without activating saved hardware outputs.
2. Create your device layout or open an exported profile using the editor. Inspect the Connections page and configure the supported devices on this PC. Saving connection settings and applying them are separate actions. See [connection behavior](https://github.com/malyarq/prismestra/blob/codex/native-design/docs/universal-connections.md).
3. Save your profile as `data/profile.json` inside the extracted folder. The launcher does not guess which of your scenes to use and does not automatically save unsaved profile edits. Use Export to move a profile with its LCD media to another PC; opening that exported package imports media into the local asset store. Save the imported profile at `data/profile.json` afterward.
4. Exit the resident session from the tray before changing launch modes. Run `Start saved scene.cmd` to open the saved profile and activate its saved connections, including configured temperature routes. Later launches use the same saved profile and state directory.

`Open Prismestra.cmd` loads `data/profile.json` when it exists; otherwise it opens the default preview. It never adds `--start-connected`. `Start saved scene.cmd` requires both that profile and `data/state/lighting-local.json`. A missing file stops launch with an instruction rather than selecting another device. Rust still validates the profile, bindings, device identities and capabilities at runtime. Passing launcher checks does not prove that a device is connected or compatible.

Closing the editor leaves the resident engine in the tray. Exit from its menu to stop the session. A repeated launch using the same state directory is handled by the application's single-instance guard; it does not switch a running preview into connected mode. Save changes and exit before restarting in the other mode.

## What moves with the folder

| Location | Contents |
| --- | --- |
| `data/profile.json` | Saved scene, layout, capture and LCD configuration |
| `data/state/` | Local connection bindings, sensor routes, imported media, scene library and editor state |
| Windows Credential Manager | Home Assistant tokens, outside the folder |

The release ZIP contains no personal profile or state. Your populated folder contains private device details; do not upload or redistribute it. A profile exported from the editor excludes local bindings and credentials. On another PC, inspect or replace local connections and temperature sources before activating outputs. Home Assistant credentials must be entered on that PC. Device identity checks are retained; copying files does not grant compatibility with a replacement controller.

## OpenRGB and prerequisites

For devices using the OpenRGB adapter, start your own compatible OpenRGB SDK6 server and configure its endpoint in Connections. Direct BLE, Skydimo, LCD, WLED and Home Assistant paths do not become OpenRGB devices through this launcher. Another application's ownership can prevent activation. Keep your fan and pump software running; Prismestra does not control cooling.

The package retains the existing Windows x64, Visual C++ v14 and WebView2 requirements described in `START-HERE.md`. To use the classic editor, run `powershell.exe -NoProfile -ExecutionPolicy Bypass -File ".\Launch Prismestra.ps1" -ClassicEditor` from the extracted folder; add `-Connected` only to activate the saved scene. The launchers do not install missing runtimes or change machine policy.

Running `Prismestra.exe` directly uses the existing per-user default state directory instead of `data/state`. Do not launch another state directory against devices already owned by a running session. Keep the application and its data together when updating; exit the session first, back up your data, and replace program files without overwriting `data`.

## Check a launch without activating anything

From the extracted folder:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File ".\Launch Prismestra.ps1" -Check
powershell.exe -NoProfile -ExecutionPolicy Bypass -File ".\Launch Prismestra.ps1" -Connected -Check
```

These commands report the planned executable and arguments without starting Prismestra or contacting devices. They do not test state-directory writability or validate remote endpoints, credentials, controller firmware or physical output. Normal launch probes state write access. Launcher failures return a nonzero exit code and the `.cmd` window stays open so its message can be read. The launcher observes immediate process exit for 1.5 seconds, then exits while Prismestra continues; it is not a background watchdog. Later application errors are reported by Prismestra.

Source: [launcher](../scripts/portable/Launch%20Prismestra.ps1), [startup and validation](../src/ui/windows.rs), [portable profile handling](../src/app/bundle.rs). Launcher and packaging checks run without physical devices in [Windows CI](../.github/workflows/windows.yml).
