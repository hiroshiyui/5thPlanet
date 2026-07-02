5thPlanet — an accuracy-first SEGA Saturn emulator
==================================================

This is a self-contained binary package of 5thPlanet's frontend, `jupiter`
(the project is named after the 5th planet — same body, two names).

SDL3 is statically embedded in the executable, so no system SDL library is
required. Keep the directory layout of this archive intact.

IMPORTANT — what you must supply yourself
-----------------------------------------
Saturn BIOS images and game discs are copyrighted and are NOT included.

  bios/   Put your own legally-obtained Saturn BIOS dump here.
          See bios/README.md for the expected filenames (the region is
          autodetected from the name) and known-good checksums.

  roms/   Put your own game disc images here (.cue/.bin preferred; .iso and
          CloneCD .ccd/.img also load). See roms/README.md for how to dump
          your own discs and the format details.

Quick start
-----------
  ./jupiter-linux-x64 bios/<your-bios>.bin                 # boot to the BIOS
  ./jupiter-linux-x64 bios/<your-bios>.bin roms/<game>.cue # boot a game

While running, press Esc to open the on-screen menu: load a disc, save/load
states, and change video / audio / controller / region settings.

Self-diagnostics:

  ./jupiter-linux-x64 doctor

runs the built-in accuracy check battery headlessly (exit code 0 = pass).

Configuration
-------------
Settings are saved to a portable `jupiter.toml` next to the executable (it is
created on first run; `jupiter.toml.example` documents every key). If no
portable file exists and none can be created, the global
`$XDG_CONFIG_HOME/5thplanet/jupiter.toml` is used instead.

Licences and integrity
----------------------
  LICENSE            5thPlanet, MIT licence
  LICENSE-SDL3.txt   the statically-embedded SDL3, zlib licence
  SHA256SUMS         checksums of every file in this archive
                     (verify with: sha256sum -c SHA256SUMS)

Project home: https://github.com/hiroshiyui/5thPlanet
