# Current default headless search example

The worked full-loop example created a fresh scratch profile but used the legacy
Super+K binding. Current ShortcutConfig defaults bind Search to Super+Space;
the K tuple is retained in the legacy binding table. Strict current-head K probes
fail to open the palette with and without the initial wheel click.

The example now holds Super through the existing key_down/key_up control path
and presses Space. All original global-scope, typed-query, visible semantic-input,
nonblank-pixel and Esc-removal assertions are retained. Both the explicit modifier
mask and held-key Space journeys passed on compositor 8f628b5154a0ae5b1de044e97bbba266e9bac2cd.
The changed tracked example itself passed with private output paths supplied by
its test harness; implementation commit is 76d7dfbce45b8b8535d1f32ec5e5637cab2140ee.
Private processes, scratch profiles and control sockets were cleaned up.

Three inspected native Library images record the held-K failure, the changed
tracked Space example with Files entered in the global palette, and its cancelled
frame. artifacts.json retains original Library identities, byte counts, dimensions,
checksums, compositor revision and the exact journey/script provenance. PNGs remain
outside Git. This corrects the earlier inventory diagnosis of a desktop search gap:
no preference, grant, credentials or input-policy change is needed.

This evidence uses an actual private CPU headless compositor. VM/device coverage
is separate: the runner VM refuses SSH at 127.0.0.1:2222 and doctor reports that it
cannot open /dev/kvm for reading and writing. No configuration was changed.
Normal CI, protected landing and merged-main example postchecks remain gates.

After rebasing the example onto menu merged-main 499f0740, its tracked source also passed against a freshly rebuilt compositor at that exact main commit. The repeated pointer open/type/Esc journey passed there too. Menu exact-main verification passed six focused cases, shell test compilation and six byte-identical capture comparisons. Original image provenance above remains unchanged.
