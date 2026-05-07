<section class="manual-sheet" id="source-code" markdown="1">

# Getting the Source Code

The firmware source code is distributed as release archives attached to the [latest GitHub release]({{ site.github.repository_url }}/releases/latest).

## Linux and macOS

Download the tarball (or zip) from the release page, then extract it using either method:

**File manager:** Double-click the archive to open it in your file manager, then extract it to a folder of your choice.

**Terminal:**

```console
tar -xf kiwi-firmware-*.tar.gz
cd kiwi-firmware-*/
```

## Windows

Download the **zip** from the release page, then extract it using either method:

**File Explorer:** Right-click the zip file and select <ui-menu>Extract All…</ui-menu>, choose a destination folder, and click <ui-btn>Extract</ui-btn>.

**PowerShell:**

```powershell
Expand-Archive kiwi-firmware-*.zip -DestinationPath .
cd kiwi-firmware-*\
```

All subsequent commands in this guide should be run from the extracted repository root.

</section>
