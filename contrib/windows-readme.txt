Nemo for Windows (preview)
==========================

Search screenshots by text or visual content, and organize folders by name.

Getting started
---------------

The easiest way is the one-line install in PowerShell, which does all of
this for you and adds Nemo to the Start menu:

    irm https://raw.githubusercontent.com/xevrion/gyotaku/main/install.ps1 | iex

By hand:

1. Keep all the files together in one folder you won't delete, for
   example C:\Users\you\AppData\Local\Programs\gyotaku:
     gyotaku-app.exe   the search window
     gyotaku.exe       the background reader and command line
     onnxruntime.dll   Microsoft's ONNX Runtime, used to read text
     msvcp140*.dll, vcruntime140*.dll
                       Microsoft's Visual C++ runtime, which ONNX Runtime
                       needs and a fresh Windows doesn't have
2. Run gyotaku-app.exe. The first time, Nemo asks which folders to read
   (Pictures\Screenshots is where Windows saves Win+PrtScn and the
   Snipping Tool's screenshots) and whether to keep reading new ones in
   the background. Saying yes also starts Nemo when you sign in.
3. Press Alt+Shift+S anywhere to open or close the search window.

The first run downloads the OCR and visual-search models once. Screenshot
search runs locally. Folder categorization sends filenames to TypeSafe
after you choose Analyze folder; optional image mode also uploads supported
image previews. File moves happen locally after you review and confirm them.

Windows may warn that the app is from an unknown publisher, since it isn't
signed yet. Choose "More info" and then "Run anyway".

To remove it: installed with the setup, uninstall Nemo from Settings, Apps.
Otherwise turn off background reading in settings (Ctrl+,), quit (Ctrl+Q),
and delete the folder. The index lives in %LOCALAPPDATA%\gyotaku and
%APPDATA%\gyotaku.

Problems and ideas: https://github.com/xevrion/gyotaku/issues
