# Contributing

Prismestra is an early Windows lighting project. Useful contributions include a reproducible bug, a tested device report, a bounded protocol adapter or a focused fix.

Before adding a new backend, check whether OpenRGB, WLED or Home Assistant already exposes the required operation. Keep discovery separate from activation. Treat transport submission and physical device confirmation as different outcomes. Never add fan or pump writes to a lighting adapter.

For a device report, include its exact model, revision, firmware, connection mode, backend version and the operations you observed. Remove tokens, serial numbers, MAC addresses, private network details and personal paths from attached logs. Do not upload firmware, vendor binaries or captures you cannot redistribute.

For a code change, describe the concrete problem and resulting behavior. Keep the patch focused and preserve upstream licenses and fixture provenance. New transport tests should use owned temporary files or loopback endpoints, with bounded I/O and a reliable exit path. Physical tests require an explicitly selected device and a recovery path.

Run the [development checks](README.md#development) before submitting a pull request. English commits should describe a finished change, for example `Preserve matrix holes in OpenRGB discovery`.
