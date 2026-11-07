Bardwave - ambience, music and sound effects for your table, streamed to the
players' browsers.  https://github.com/adamantida/Bardwave

QUICK START
1. Keep the program and config.toml in the same folder (any folder you can
   write to, e.g. Documents\Bardwave - not Program Files).
2. Start Bardwave (Bardwave.exe on Windows, ./bardwave on Linux).
3. ADD SOUND: pick your MP3 / OGG / FLAC / WAV files. Group them by scene.
4. START SERVER, then COPY LINK and send it to the players. They open it in
   any browser and press "Establish Link".

THE LINK
The link ends with ?k=<key>. Without the key nobody can listen, even when the
port is open to the internet. The refresh button next to "Access key" makes a
new key: the old link stops working and everyone on it is disconnected.

PLAYERS OUTSIDE YOUR NETWORK
On start Bardwave asks the router (UPnP) to forward the port and shows an
INTERNET link. It does not work while a VPN on this PC routes all traffic, and
Windows may block it until you allow Bardwave for public networks too (or run
it once as administrator). Test the internet link from a phone on mobile data.

WITHOUT A WINDOW
  Bardwave.exe --server-only
loads the saved library and prints two links: the one for the players and a
control page (http://127.0.0.1:<port>/control) that works only on this PC.

LINUX
Needs the usual desktop libraries: GTK 3, ALSA, libxkbcommon
(Debian/Ubuntu: sudo apt install libgtk-3-0 libasound2 libxkbcommon0).
