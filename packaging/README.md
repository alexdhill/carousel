# Packaging — `.deck` file association

Carousel opens a `.deck` passed on the command line (`Carousel foo.deck`) and,
on macOS, via Finder's open-file event. These files register the association so
double-clicking a `.deck` launches the app. They are **inert until installed**
by real packaging — see "Follow-up (C)".

IDs used throughout:

- Bundle id: `xyz.alexdhill.carousel`
- macOS UTI: `xyz.alexdhill.carousel.deck`
- Linux MIME: `application/x-carousel-deck`

## macOS

`macos/Info.plist` declares the document type + exported UTI. It only takes
effect inside an `.app` bundle:

```
Carousel.app/Contents/
    Info.plist            <- macos/Info.plist
    MacOS/Carousel        <- the built binary
```

After placing the `.app`, refresh Launch Services:

```
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f Carousel.app
```

## Linux

Install the MIME type, the desktop entry, then update the caches:

```
install -Dm644 linux/carousel-deck.xml ~/.local/share/mime/packages/carousel-deck.xml
install -Dm644 linux/carousel.desktop  ~/.local/share/applications/carousel.desktop
# ensure the Carousel binary is on PATH (Exec=Carousel %f)
update-mime-database ~/.local/share/mime
update-desktop-database ~/.local/share/applications
```

## Windows

Edit `windows/carousel-deck.reg`, replacing `C:\Path\To\Carousel.exe` with the
installed path, then import:

```
reg import windows\carousel-deck.reg
```

(`HKEY_CURRENT_USER` — per-user, no admin needed. An installer would write the
same keys.)

## Follow-up (C)

Not done yet: wiring these into CI so releases ship an installable artifact —
a macOS `.app` (embedding `Info.plist`), a Windows installer (writing the
registry keys), and a Linux package/AppImage (installing the `.desktop` +
MIME). Until then these are applied manually for testing.
