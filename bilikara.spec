# -*- mode: python ; coding: utf-8 -*-


a = Analysis(
    ['C:\\bilikaradev\\bilikara\\start_bilikara.py'],
    pathex=[],
    binaries=[('C:\\ffmpeg\\ffmpeg.exe', 'vendor')],
    datas=[('C:\\bilikaradev\\bilikara\\static', 'static'), ('C:\\bilikaradev\\bilikara\\APP_VERSION', '.')],
    hiddenimports=[],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[],
    noarchive=False,
    optimize=0,
)
pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name='bilikara',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,
    console=False,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
)
coll = COLLECT(
    exe,
    a.binaries,
    a.datas,
    strip=False,
    upx=True,
    upx_exclude=[],
    name='bilikara',
)
