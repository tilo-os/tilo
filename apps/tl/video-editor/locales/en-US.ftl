# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-video-editor = Video Editor

d-so-tilo-video-editor-keywords = video editor, video, film, timeline, cut, trim, split, transition, title, export, videobearbeitung, video schneiden, film, zeitleiste, schnitt, exportieren

d-jqhqav7h = Media
    .doc = One source in the bin: what it is, where its bytes are, and — for a title — the words it draws.
    .noun = { $count ->
        [one] media
       *[other] media
    }
    .untitled = Untitled source

d-efejoszz = Name

d-pcamqw4n = Form

d-wj2aohfd = Duration

d-hpkuwe3q = Width

d-krrrznuy = Height

d-vmxn3cwg = Fps

d-foxa6zt7 = Palette
    .doc = Its place in the palette, which tints its bin swatch and its clips.

d-zltur6gc = Path
    .doc = The address its bytes read at (`tilo-blob://…`); empty for a title.

d-ap457hnd = Has audio

d-3vgrlwgv = Still
    .doc = Its picture as the bin shows it, kept once the engine drew it.

d-vddvspwk = Peaks
    .doc = Its loudness across the whole file, a fraction of full scale, as the waveform draws it.

d-e7atusb5 = Words
    .doc = A title's words.

d-qrihyxs3 = Size
    .doc = A title's cap height, a fraction of the frame height.

d-x2yorlvh = Red

d-5nv4umhz = Green

d-ktanswey = Blue

d-lqpu2kuo = Alpha

d-3j5mkeir = Align

d-u37npdvi = Bold

d-wzbpbjsf = Italic

d-hlbhw6tg = Backdrop
    .doc = A title's backing plate, 0 for none.

d-adomtjkz = Effect
    .doc = One stackable picture effect on a clip: what it is and how strong.
    .noun = { $count ->
        [one] effect
       *[other] effects
    }
    .untitled = Effect

d-ktghxc36 = Form

d-lj3ggezn = Amount

d-adomtjkz-filter-amount = Amount

d-2um277kp = Track
    .doc = A row of the timeline.
    .noun = { $count ->
        [one] track
       *[other] tracks
    }
    .untitled = Track

d-hyiqyr2d = Name

d-voeh7uw2 = Form

d-h5vj7qoc = Index

d-o7i5df56 = Muted

d-azpfbjsn = Solo

d-wowizwqg = Locked

d-rofejlx6 = Gain db
    .doc = The fader, in decibels, 0 at unity.

d-ovon2nm7 = Pan
    .doc = Where it sits across the stereo field: -1 hard left, 0 centre, 1 hard right.

d-2um277kp-filter-form = Form

d-2um277kp-filter-index = Index

d-2um277kp-filter-muted = Muted

d-2um277kp-filter-solo = Solo

d-2um277kp-filter-locked = Locked

d-2um277kp-filter-gain_db = Gain db

d-2um277kp-filter-pan = Pan

d-rggqngof = Key
    .doc = One value a clip holds at a moment of its own, and how the curve leaves it.
    .noun = { $count ->
        [one] key
       *[other] keys
    }
    .untitled = Key

d-362ioruy = Param
    .doc = The field it drives, as the engine names it (`opacity`, `pos_x`, `effect:…`).

d-ksxcsp7k = T
    .doc = Seconds from the clip's start, so a move or a split carries it.

d-al7ic2wh = V

d-4rmokhsj = Interp
    .doc = `linear`, `hold` or `ease`.

d-rggqngof-filter-t = T

d-rggqngof-filter-v = V

d-aj2coyre = Clip
    .doc = One placement of a source on a track: where it starts, which part of the source it shows, and how it looks and sounds.
    .noun = { $count ->
        [one] clip
       *[other] clips
    }
    .untitled = Clip

d-azbufhus = Name

d-tpxg6k4q = Track

d-y4qdvmxb = Media

d-vle43xl6 = Start
    .doc = Its left edge on the timeline, in seconds.

d-tmrpa5lj = In point
    .doc = The source in-point, in seconds into the source.

d-ijg7zj64 = Out point
    .doc = The source out-point, in seconds into the source.

d-mzmglpkg = Opacity

d-hb6kydcz = Volume

d-5lwntoqr = Transition
    .doc = The incoming transition: empty for a hard cut, else `dissolve`, `fade`, `dip` or `wipe`.

d-ihl7lqbw = Transition duration
    .doc = How long that transition runs, in seconds; 0 for its default.

d-fmhqhubf = Fade in

d-bygbi2tw = Fade out

d-jrsfcsej = Palette

d-55seasjt = Speed rate
    .doc = The playback rate; 1 is normal.

d-dmthewcr = Reverse

d-ttu42344 = Brightness

d-sosquagj = Contrast

d-t6mekjo2 = Saturation

d-kikxhud5 = Scale x

d-67q6ewpx = Scale y

d-nu7xhxvx = Pos x

d-vrqmvalw = Pos y

d-3ru7tn4m = Rotation

d-kx3c765z = Crop left

d-4rl2tdsz = Crop right

d-d4ryk5ir = Crop top

d-r27hvfe7 = Crop bottom

d-7uz3gyat = Effects

d-tskiyma5 = Key enabled

d-nflhw3wz = Key tolerance

d-gk3fgv4a = Key softness

d-zxs2bxu6 = Key spill

d-ag6tnt5w = Keys
    .doc = Where a value of this clip changes over its own length: a curve on a field keeps the constant field's value where no key sits near.

d-54ikwabs = Strips
    .doc = Frames of this clip as the inspector's filmstrip shows them, and the span they were drawn for, so a trim draws them again.

d-bvukpc5x = Strips key

d-dhh754gx = Key red
    .doc = The colour the key removes, in 0..1 RGB: what a sample from the frame sets.

d-podots7u = Key green

d-tgpgd3hb = Key blue

d-hfnhir7p = Mask shape
    .doc = A mask that limits where the layer shows: empty for none, else `rect` or `ellipse`.

d-hclucc6k = Mask x

d-g2n4egth = Mask y

d-lwnm5qmy = Mask w

d-casg7gsr = Mask h

d-u3oht37u = Mask rotation

d-iraobc5s = Mask feather

d-6njmwsei = Mask invert

d-vmc7izdi = Link
    .doc = The link group this clip is one half of: its picture and its sound move together.

d-wovh5j2b = Link follows

d-poy7f6dc = Sound split
    .doc = True when this picture's own sound was split off onto an audio clip.

d-aj2coyre-filter-track = Track

d-aj2coyre-filter-media = Media

d-aj2coyre-filter-start = Start

d-aj2coyre-filter-in_point = In point

d-aj2coyre-filter-out_point = Out point

d-aj2coyre-filter-opacity = Opacity

d-aj2coyre-filter-volume = Volume

d-aj2coyre-filter-transition_duration = Transition duration

d-aj2coyre-filter-fade_in = Fade in

d-aj2coyre-filter-fade_out = Fade out

d-aj2coyre-filter-palette = Palette

d-aj2coyre-filter-speed_rate = Speed rate

d-aj2coyre-filter-reverse = Reverse

d-aj2coyre-filter-brightness = Brightness

d-aj2coyre-filter-contrast = Contrast

d-aj2coyre-filter-saturation = Saturation

d-aj2coyre-filter-scale_x = Scale x

d-aj2coyre-filter-scale_y = Scale y

d-aj2coyre-filter-pos_x = Pos x

d-aj2coyre-filter-pos_y = Pos y

d-aj2coyre-filter-rotation = Rotation

d-aj2coyre-filter-crop_left = Crop left

d-aj2coyre-filter-crop_right = Crop right

d-aj2coyre-filter-crop_top = Crop top

d-aj2coyre-filter-crop_bottom = Crop bottom

d-aj2coyre-filter-key_enabled = Key enabled

d-aj2coyre-filter-key_tolerance = Key tolerance

d-aj2coyre-filter-key_softness = Key softness

d-aj2coyre-filter-key_spill = Key spill

d-aj2coyre-filter-key_red = Key red

d-aj2coyre-filter-key_green = Key green

d-aj2coyre-filter-key_blue = Key blue

d-aj2coyre-filter-mask_x = Mask x

d-aj2coyre-filter-mask_y = Mask y

d-aj2coyre-filter-mask_w = Mask w

d-aj2coyre-filter-mask_h = Mask h

d-aj2coyre-filter-mask_rotation = Mask rotation

d-aj2coyre-filter-mask_feather = Mask feather

d-aj2coyre-filter-mask_invert = Mask invert

d-aj2coyre-filter-link_follows = Link follows

d-aj2coyre-filter-sound_split = Sound split

d-kicr3edk = Marker
    .doc = A point on the timeline a person marked.
    .noun = { $count ->
        [one] marker
       *[other] markers
    }
    .untitled = Marker

d-6gy3r5jn = Name

d-43eckzwd = T
    .doc = Seconds on the timeline.

d-jvjfto7y = Palette

d-h2qmhyqy = Note

d-kicr3edk-filter-t = T

d-kicr3edk-filter-palette = Palette

d-fkdizcdd = Project
    .doc = A cut: its shape, its sources, its tracks and clips, and what the engine last drew of it.
    .noun = { $count ->
        [one] project
       *[other] projects
    }
    .untitled = Untitled project

d-hdhd7ufr = Name

d-sggyvq5o = Fps

d-rv7tdvl6 = Width

d-z7cmqzxo = Height

d-m7e2vebr = Explicit format
    .doc = Whether the shape was chosen by a person rather than taken from the first source.

d-enmbeevc = Mark in
    .doc = Where a render starts and ends on the timeline; negative for the head and the end.

d-vfq5phqm = Mark out

d-f7xzwsye = Format
    .doc = The container an export writes.

d-qm4juanr = Media

d-4z3qlwar = Tracks

d-74vaoosx = Clips

d-qr6qe3cp = Markers

d-47vsdgaf = Still
    .doc = The frame the preview shows at `still_at`, drawn by the engine.

d-dppbe5en = Still at

d-gkp4va33 = Proxy
    .doc = The cut rendered small for playing in the window.

d-xa5fpldf = Job
    .doc = The render this cut has running, and how far it has got.

d-dkhcppi4 = Job kind

d-myjdbgx2 = Job done

d-63bjrnm4 = Job total

d-rhjs5vir = Exported
    .doc = The finished file an export made.

d-4jky56y7 = Trouble
    .doc = Why the last thing the engine was asked failed.

d-fkdizcdd-filter-fps = Fps

d-fkdizcdd-filter-width = Width

d-fkdizcdd-filter-height = Height

d-fkdizcdd-filter-explicit_format = Explicit format

d-fkdizcdd-filter-mark_in = Mark in

d-fkdizcdd-filter-mark_out = Mark out

d-fkdizcdd-filter-still_at = Still at

d-fkdizcdd-filter-job_done = Job done

d-fkdizcdd-filter-job_total = Job total

d-ugegxdzp = Editor
    .doc = The workspace's cuts, and the one the window has open.
    .noun = { $count ->
        [one] editor
       *[other] editors
    }

d-ws67tq4i = Projects

d-i3vrs6nt = Open

d-qyx2uds6 = Sessions
    .doc = The session the window plays through, so its sound has a device to meter.

d-3kenkvdb = Playing

d-ugegxdzp-filter-open = Open

d-ugegxdzp-filter-playing = Playing

d-wzqywwxp = Probed
    .doc = What the device answered when it was asked what a file is.
    .noun = { $count ->
        [one] probed
       *[other] probeds
    }

d-bc2o5yk6 = Kind

d-jb7opba6 = Duration

d-duh5etpe = Width

d-ibouz53w = Height

d-eci2btbm = Fps

d-cg3roaa5 = Has audio

d-r72d7i4q = Codec

d-wzqywwxp-filter-duration = Duration

d-wzqywwxp-filter-width = Width

d-wzqywwxp-filter-height = Height

d-wzqywwxp-filter-fps = Fps

d-wzqywwxp-filter-has_audio = Has audio

d-mxktylr3 = Probe ask
    .noun = { $count ->
        [one] probe ask
       *[other] probe asks
    }

d-dx4zjyh5 = Url

d-q4b7gd4q = Still ask
    .noun = { $count ->
        [one] still ask
       *[other] still asks
    }

d-itbf6wf4 = Project
    .doc = The cut, as `json.encode(shot(p))` writes it.

d-5klklgrm = At

d-zrjp2ayo = Width

d-c7hx2nde = Height

d-q4b7gd4q-filter-at = At

d-q4b7gd4q-filter-width = Width

d-q4b7gd4q-filter-height = Height

d-t53sw4me = Still answer
    .noun = { $count ->
        [one] still answer
       *[other] still answers
    }

d-fqmupsg5 = Url

d-xkpsjf7l = Wave ask
    .noun = { $count ->
        [one] wave ask
       *[other] wave asks
    }

d-hv2hn45e = Url

d-u2gsuh7h = Buckets

d-xkpsjf7l-filter-buckets = Buckets

d-oeiycvc3 = Peaks
    .noun = { $count ->
        [one] peak
       *[other] peaks
    }

d-xatqduw2 = Peaks

d-khrei3pe = Render ask
    .noun = { $count ->
        [one] render ask
       *[other] render asks
    }

d-4xhy5qpi = Project
    .doc = The cut, as `json.encode(shot(p))` writes it.

d-nlg2n4yg = Resolution

d-n3bktwt4 = Container

d-ylbeg2vl = Fps

d-mqmxnv4s = Name

d-khrei3pe-filter-fps = Fps

d-cvn34nlm = Job answer
    .noun = { $count ->
        [one] job answer
       *[other] job answers
    }

d-2qflgu2y = Job

d-cfr4gprt = Job state
    .noun = { $count ->
        [one] job state
       *[other] job states
    }

d-2lqqe2u4 = State

d-wddixskv = Done

d-yfo7z67v = Total

d-ivfsfn6q = Url

d-tlb73mjx = Error

d-cfr4gprt-filter-done = Done

d-cfr4gprt-filter-total = Total

d-7ufzdmfk = Shot
    .doc = The cut as the device reads it: plain values, with the names the engine knows.
    .noun = { $count ->
        [one] shot
       *[other] shots
    }

d-fagho4gr = Name

d-fsxs4zep = Fps

d-ebteysdk = Width

d-a4torznk = Height

d-v6ck4d4l = Mark in

d-hzxujhk3 = Mark out

d-yjp3sehi = Media
    .doc = One JSON text a source; each is a `ShotMedia` written out.

d-q3ecfgq3 = Tracks
    .doc = One JSON text a track; each is a `ShotTrack` written out.

d-wgoywspl = Clips
    .doc = One JSON text a clip; each is a `ShotClip` written out.

d-w26d474g = Markers
    .doc = One JSON text a marker; each is a `ShotMarker` written out.

d-7ufzdmfk-filter-fps = Fps

d-7ufzdmfk-filter-width = Width

d-7ufzdmfk-filter-height = Height

d-7ufzdmfk-filter-mark_in = Mark in

d-7ufzdmfk-filter-mark_out = Mark out

d-wxyo7lsu = Shot media
    .noun = { $count ->
        [one] shot media
       *[other] shot media
    }

d-jws44pym = Id

d-6irok5cm = Name

d-2yxap4ne = Kind

d-j3rpcgim = Duration

d-pwfziujj = Width

d-7cokd24o = Height

d-ldk5mv2m = Fps

d-5gvee3he = Palette

d-betxnhkz = Path

d-7ppacpgk = Has audio

d-cosd36fu = Content
    .doc = A title's words; empty for every decoded source.

d-tmz6b4vk = Size

d-uk5cqaxq = Color

d-a5zwa247 = Align

d-mjhc4g63 = Font

d-lykfeg5x = Bold

d-npxtqckt = Italic

d-4b6rl7fz = Backdrop

d-wxyo7lsu-filter-duration = Duration

d-wxyo7lsu-filter-width = Width

d-wxyo7lsu-filter-height = Height

d-wxyo7lsu-filter-fps = Fps

d-wxyo7lsu-filter-palette = Palette

d-wxyo7lsu-filter-has_audio = Has audio

d-wxyo7lsu-filter-size = Size

d-wxyo7lsu-filter-bold = Bold

d-wxyo7lsu-filter-italic = Italic

d-wxyo7lsu-filter-backdrop = Backdrop

d-szcty4av = Shot track
    .noun = { $count ->
        [one] shot track
       *[other] shot tracks
    }

d-wzjt7soo = Id

d-2phnzuqk = Name

d-mb6mtsoz = Kind

d-jr2eyeqg = Index

d-wyqopmjv = Muted

d-3enbjrqj = Solo

d-lr3pdaah = Locked

d-zse3uh5c = Gain db

d-aix6cf7x = Pan

d-szcty4av-filter-index = Index

d-szcty4av-filter-muted = Muted

d-szcty4av-filter-solo = Solo

d-szcty4av-filter-locked = Locked

d-szcty4av-filter-gain_db = Gain db

d-szcty4av-filter-pan = Pan

d-twd6225m = Shot clip
    .noun = { $count ->
        [one] shot clip
       *[other] shot clips
    }

d-jeeiaroo = Id

d-akxlh23d = Track

d-pptlby5v = Media

d-3dgm7b2b = Start

d-zbz5amfa = In point

d-pkiaohdd = Out point

d-xi2qtarh = Opacity

d-chhkku2t = Volume

d-s2fdck6n = Transition

d-rrf4qn3g = Transition duration

d-xubtbe5w = Fade in

d-jllwurew = Fade out

d-clwasyxv = Palette

d-uge3tmju = Speed rate

d-rxbq7fqf = Speed reverse

d-5utmqjdy = Brightness

d-5kf4asry = Contrast

d-ugpps35t = Saturation

d-ueazn2zi = Scale x

d-qxxmhhdr = Scale y

d-hl2h6f76 = Pos x

d-nhestloi = Pos y

d-ckydsf7y = Rotation

d-ttpiryuy = Crop left

d-dazt7wt5 = Crop right

d-o5353t6h = Crop top

d-lsgktf3d = Crop bottom

d-mq4hgil7 = Effects
    .doc = One JSON text an effect, in stack order.

d-6ahtwc73 = Keys
    .doc = One JSON text a key, in clip-local seconds.

d-2i54q4ly = Key enabled

d-hcm6xmnu = Key color

d-ss3pryvp = Key tolerance

d-odmxpb6p = Key softness

d-vwxtiqh4 = Key spill

d-v6saof6k = Mask shape

d-drrsji3r = Mask x

d-q63mhtzf = Mask y

d-w3owjsm6 = Mask w

d-lrguhlws = Mask h

d-u4sfvd6p = Mask rotation

d-zteue6rc = Mask feather

d-ah7jtdcc = Mask invert

d-2wno4yo2 = Link

d-3rzhv2vf = Link follows

d-hulnlv2t = Sound split

d-twd6225m-filter-start = Start

d-twd6225m-filter-in_point = In point

d-twd6225m-filter-out_point = Out point

d-twd6225m-filter-opacity = Opacity

d-twd6225m-filter-volume = Volume

d-twd6225m-filter-transition_duration = Transition duration

d-twd6225m-filter-fade_in = Fade in

d-twd6225m-filter-fade_out = Fade out

d-twd6225m-filter-palette = Palette

d-twd6225m-filter-speed_rate = Speed rate

d-twd6225m-filter-speed_reverse = Speed reverse

d-twd6225m-filter-brightness = Brightness

d-twd6225m-filter-contrast = Contrast

d-twd6225m-filter-saturation = Saturation

d-twd6225m-filter-scale_x = Scale x

d-twd6225m-filter-scale_y = Scale y

d-twd6225m-filter-pos_x = Pos x

d-twd6225m-filter-pos_y = Pos y

d-twd6225m-filter-rotation = Rotation

d-twd6225m-filter-crop_left = Crop left

d-twd6225m-filter-crop_right = Crop right

d-twd6225m-filter-crop_top = Crop top

d-twd6225m-filter-crop_bottom = Crop bottom

d-twd6225m-filter-key_enabled = Key enabled

d-twd6225m-filter-key_tolerance = Key tolerance

d-twd6225m-filter-key_softness = Key softness

d-twd6225m-filter-key_spill = Key spill

d-twd6225m-filter-mask_x = Mask x

d-twd6225m-filter-mask_y = Mask y

d-twd6225m-filter-mask_w = Mask w

d-twd6225m-filter-mask_h = Mask h

d-twd6225m-filter-mask_rotation = Mask rotation

d-twd6225m-filter-mask_feather = Mask feather

d-twd6225m-filter-mask_invert = Mask invert

d-twd6225m-filter-link_follows = Link follows

d-twd6225m-filter-sound_split = Sound split

d-ta4fbzv3 = Shot effect
    .noun = { $count ->
        [one] shot effect
       *[other] shot effects
    }

d-ai7thy2p = Id

d-seoccvzw = Form

d-y2yjk3ou = Amount

d-ta4fbzv3-filter-amount = Amount

d-4ashukng = Shot key
    .noun = { $count ->
        [one] shot key
       *[other] shot keys
    }

d-fpeyg4lm = Param

d-frbcwy5c = T

d-hxcqnoii = V

d-pf3f6zou = Interp

d-4ashukng-filter-t = T

d-4ashukng-filter-v = V

d-uwrwmii3 = Thumb ask
    .doc = One source's picture at `at`, kept: a filmstrip frame.
    .noun = { $count ->
        [one] thumb ask
       *[other] thumb asks
    }

d-zbk6etmx = Url

d-mom4w6zg = At

d-qdxfi3dv = Width

d-a4wvyrpr = Height

d-uwrwmii3-filter-at = At

d-uwrwmii3-filter-width = Width

d-uwrwmii3-filter-height = Height

d-3eadse7q = Sample ask
    .doc = The colour of the cut's frame at (x, y), in fractions of its box.
    .noun = { $count ->
        [one] sample ask
       *[other] sample asks
    }

d-47t7hgmm = Project

d-lph6xeeo = At

d-hpbggrop = X

d-nklgpqyv = Y

d-3eadse7q-filter-at = At

d-3eadse7q-filter-x = X

d-3eadse7q-filter-y = Y

d-24bzwas5 = Sample answer
    .noun = { $count ->
        [one] sample answer
       *[other] sample answers
    }

d-twwt37df = Red

d-6j7i6jgf = Green

d-qbxkptqm = Blue

d-24bzwas5-filter-red = Red

d-24bzwas5-filter-green = Green

d-24bzwas5-filter-blue = Blue

d-ofec2skm = Shot marker
    .noun = { $count ->
        [one] shot marker
       *[other] shot markers
    }

d-5evudwdu = Id

d-bh2yiei5 = T

d-ajyxetst = Name

d-2wvqaalc = Palette

d-ueznappo = Note

d-ofec2skm-filter-t = T

d-ofec2skm-filter-palette = Palette

d-rsaogmjm = Saved
    .doc = A project file as it is written: what a person hands to someone else.
    .noun = { $count ->
        [one] saved
       *[other] saveds
    }

d-y5f2zb7y = Name

d-pfj7sab5 = Fps

d-4nw6mpgl = Width

d-tyqdqd4o = Height

d-idvvsvmm = Mark in

d-dadn3d73 = Mark out

d-hzulw2ku = Media
    .doc = One JSON text a source.

d-sgtdffn3 = Tracks
    .doc = One JSON text a track.

d-tgtkyvzv = Clips
    .doc = One JSON text a clip.

d-mbkqck5v = Markers
    .doc = One JSON text a marker.

d-rsaogmjm-filter-fps = Fps

d-rsaogmjm-filter-width = Width

d-rsaogmjm-filter-height = Height

d-rsaogmjm-filter-mark_in = Mark in

d-rsaogmjm-filter-mark_out = Mark out

d-bykmbwv6 = Saved media
    .noun = { $count ->
        [one] saved media
       *[other] saved media
    }

d-y4ivogi5 = Name

d-4yybplx2 = Form

d-lrk7dehr = Duration

d-zzc32v7l = Width

d-qj5gefuh = Height

d-23dpu2sh = Fps

d-soogrhvl = Palette

d-ooln2ibo = Path

d-ydskn4p7 = Has audio

d-im5ank6o = Words

d-tmrphs4y = Size

d-kqelnlf5 = Red

d-vikauvue = Green

d-l67cmsns = Blue

d-ozd3qrh5 = Alpha

d-ytkxp4t4 = Align

d-oecwbkzv = Bold

d-qefzzmnw = Italic

d-npetd5wt = Backdrop

d-bykmbwv6-filter-duration = Duration

d-bykmbwv6-filter-width = Width

d-bykmbwv6-filter-height = Height

d-bykmbwv6-filter-fps = Fps

d-bykmbwv6-filter-palette = Palette

d-bykmbwv6-filter-has_audio = Has audio

d-bykmbwv6-filter-size = Size

d-bykmbwv6-filter-red = Red

d-bykmbwv6-filter-green = Green

d-bykmbwv6-filter-blue = Blue

d-bykmbwv6-filter-alpha = Alpha

d-bykmbwv6-filter-bold = Bold

d-bykmbwv6-filter-italic = Italic

d-bykmbwv6-filter-backdrop = Backdrop

d-5wot45br = Saved track
    .noun = { $count ->
        [one] saved track
       *[other] saved tracks
    }

d-hahxd2bf = Name

d-6eogjiom = Form

d-kpfeeasv = Index

d-objfxiib = Muted

d-xucxzrou = Solo

d-ziybhwce = Locked

d-mddfj6qk = Gain db

d-kmg2laxv = Pan

d-5wot45br-filter-index = Index

d-5wot45br-filter-muted = Muted

d-5wot45br-filter-solo = Solo

d-5wot45br-filter-locked = Locked

d-5wot45br-filter-gain_db = Gain db

d-5wot45br-filter-pan = Pan

d-werb5t7p = Saved clip
    .noun = { $count ->
        [one] saved clip
       *[other] saved clips
    }

d-xelrculp = Name

d-nakmpr6q = Track

d-b6xll7vm = Media

d-3styztut = Start

d-fosnfakw = In point

d-fyyrf7ca = Out point

d-kl2xarv5 = Opacity

d-xjyxglh4 = Volume

d-3ayzf2x5 = Transition

d-dqqoukz2 = Transition duration

d-ked6w3da = Fade in

d-gs542y2n = Fade out

d-xuo76wt5 = Speed rate

d-joyeavyw = Reverse

d-y7jv62ls = Brightness

d-g5rdmkth = Contrast

d-zgf5atrf = Saturation

d-r4zzuhna = Scale x

d-u3nbauiz = Scale y

d-7rxjc3mu = Pos x

d-4fkenwwm = Pos y

d-n2rqdsw7 = Rotation

d-fycn5pvk = Crop left

d-2tb4h5uc = Crop right

d-7madt25j = Crop top

d-3wy2xkbg = Crop bottom

d-2ui2wpyh = Key enabled

d-cxqyoowf = Key tolerance

d-rjd3q7ct = Key softness

d-avubfu74 = Key spill

d-smqr4d4n = Mask shape

d-yr4ncz4w = Mask x

d-7g7aeosa = Mask y

d-jasgkfww = Mask w

d-f4dke5o2 = Mask h

d-fdrernpm = Mask rotation

d-to3xcmob = Mask feather

d-6dr3jtxx = Mask invert

d-pdhb3rjf = Link

d-6oop3g3z = Keys
    .doc = One JSON text a key of the clip.

d-werb5t7p-filter-track = Track

d-werb5t7p-filter-media = Media

d-werb5t7p-filter-start = Start

d-werb5t7p-filter-in_point = In point

d-werb5t7p-filter-out_point = Out point

d-werb5t7p-filter-opacity = Opacity

d-werb5t7p-filter-volume = Volume

d-werb5t7p-filter-transition_duration = Transition duration

d-werb5t7p-filter-fade_in = Fade in

d-werb5t7p-filter-fade_out = Fade out

d-werb5t7p-filter-speed_rate = Speed rate

d-werb5t7p-filter-reverse = Reverse

d-werb5t7p-filter-brightness = Brightness

d-werb5t7p-filter-contrast = Contrast

d-werb5t7p-filter-saturation = Saturation

d-werb5t7p-filter-scale_x = Scale x

d-werb5t7p-filter-scale_y = Scale y

d-werb5t7p-filter-pos_x = Pos x

d-werb5t7p-filter-pos_y = Pos y

d-werb5t7p-filter-rotation = Rotation

d-werb5t7p-filter-crop_left = Crop left

d-werb5t7p-filter-crop_right = Crop right

d-werb5t7p-filter-crop_top = Crop top

d-werb5t7p-filter-crop_bottom = Crop bottom

d-werb5t7p-filter-key_enabled = Key enabled

d-werb5t7p-filter-key_tolerance = Key tolerance

d-werb5t7p-filter-key_softness = Key softness

d-werb5t7p-filter-key_spill = Key spill

d-werb5t7p-filter-mask_x = Mask x

d-werb5t7p-filter-mask_y = Mask y

d-werb5t7p-filter-mask_w = Mask w

d-werb5t7p-filter-mask_h = Mask h

d-werb5t7p-filter-mask_rotation = Mask rotation

d-werb5t7p-filter-mask_feather = Mask feather

d-werb5t7p-filter-mask_invert = Mask invert

d-ng75u2ju = Saved marker
    .noun = { $count ->
        [one] saved marker
       *[other] saved markers
    }

d-s6k7ckvj = Name

d-bglc57r5 = T

d-2zmyfxy2 = Palette

d-uqvihrf3 = Note

d-ng75u2ju-filter-t = T

d-ng75u2ju-filter-palette = Palette

d-4nmhy6dt = Drawn
    .doc = The timeline's shapes and, beside each, the clip it belongs to: the drawing reports the shape a press landed on, and this is what that shape was.
    .noun = { $count ->
        [one] drawn
       *[other] drawns
    }

d-jaecw4vv = Shapes

d-msyhaonn = Owners

d-mciac4w4 = Form
    .doc = What a source is: a picture that moves, a sound, a still, or words this app draws itself.

d-zblz6mlg = Video

d-ynngpmma = Audio

d-tojmog4r = Image

d-7xc3xbut = Title

d-lpnhv5tz = Align
    .doc = How a title's lines sit in its box.

d-o6rryp7r = Left

d-xogrqbl7 = Centre

d-gcbw3mrx = Right

d-y7qoj2nn = Grab
    .doc = What a drag on the timeline grabbed: nothing, a clip's body, or one of its edges.

d-gp6bczpu = Empty

d-v7pj42su = Move

d-niisiqbe = Head

d-4gi6fhjp = Tail

d-ekz7suen = Split at
    .doc = Cuts a clip in two at `at`, each half keeping the source its own part shows.

d-ekz7suen-c = C

d-ekz7suen-at = At

d-vt2rq6ml = Shifted
    .doc = Moves `c` (and its linked sound with it) to `start`, refusing an overlap.

d-vt2rq6ml-c = C

d-vt2rq6ml-start = Start

d-qz7qr2ru = Trouble

d-qz7qr2ru-why = Why

d-e7mxkoe3 = Absorb
    .doc = Asks the device what a file is, adds it as a source and draws its swatch and waveform.

d-e7mxkoe3-file = File

d-o5pyx57x = Draw swatch
    .doc = The source's picture as the bin shows it.

d-o5pyx57x-width = Width

d-csrgoth7 = Hear
    .doc = The source's loudness across the file, as the waveform draws it.

d-blq35ymn = Draw still
    .doc = The frame the preview shows at `at`, drawn by the engine.

d-blq35ymn-at = At

d-ozvvnp6f = Begin render
    .doc = Starts a render: `preview` keeps the cut small for the window to play, `export` writes the file a person saves.

d-ozvvnp6f-render_kind = Render kind

d-ozvvnp6f-resolution = Resolution

d-ozvvnp6f-container = Container

d-ozvvnp6f-fps = Fps

d-tpulmxks = New project
    .doc = Starts a cut: its shape and its first two tracks.
    .w-untitled-project = Untitled project

d-6oillx2m = Open project
    .doc = Opens the cut the window shows.

d-6oillx2m-p = P

d-heiumxwf = Session
    .doc = The session this window plays through: the one it has, or one made now.

d-msh6ollo = Rename project

d-msh6ollo-name = Name

d-p2rzlwd5 = Delete project

d-p2rzlwd5-p = P

d-jly5vvua = Import files
    .doc = Adds every file a person chose or dropped as a source.

d-jly5vvua-files = Files

d-q7ohei6b = Import one

d-q7ohei6b-file = File

d-g5zie4f5 = Open in the Video Editor
    .doc = Opens a video, a sound or a picture in a cut of its own: what Files and the person's own system offer the editor for.

d-g5zie4f5-file = File

d-kby4rl22 = Open a project
    .doc = Opens a project file a person chose: one undoable edit that makes the cut it holds.

d-kby4rl22-file = File

d-ywy6v3p3 = Save the project
    .doc = The cut as a file a person keeps: its sources and their places, where they stand on their own device.

d-b7tqrowc = Save the film
    .doc = The cut as a film: what an export made.

d-4occtx3e = Place
    .doc = Places `m` on `track` at `at`, in the first place it fits.

d-4occtx3e-m = M

d-4occtx3e-track = Track

d-4occtx3e-at = At

d-puz7vwtv = Place media
    .doc = Places the bin's source on the first track of its form, at `at`.

d-puz7vwtv-m = M

d-puz7vwtv-at = At

d-j47xlg5b = Commit drag
    .doc = What a drag of the timeline committed: a clip moved or trimmed.

d-j47xlg5b-grab = Grab

d-j47xlg5b-clip = Clip

d-j47xlg5b-from = From

d-j47xlg5b-at = At

d-sy5ehyjx = Split clip
    .doc = Cuts the clip at the playhead in two.

d-sy5ehyjx-c = C

d-sy5ehyjx-at = At

d-supdi5qb = Cut at
    .doc = Cuts every clip the playhead stands on.

d-supdi5qb-at = At

d-u7przh43 = Delete the clip
    .doc = Deletes a clip, and the sound linked to it with it.

d-u7przh43-c = C

d-lln5pwvg = Close the gap
    .doc = Deletes a clip and closes the gap it leaves behind it.

d-lln5pwvg-c = C

d-ecft5dqw = Nudge
    .doc = Slips the clip along the timeline by frames.

d-ecft5dqw-c = C

d-ecft5dqw-frames = Frames

d-ne4it5nd = Set speed
    .doc = Sets the rate a clip plays at: 2 twice as fast and half as long, 0.5 in slow motion.

d-ne4it5nd-speed = Speed

d-ig42742r = Normal speed

d-lzc5gbav = Set transition
    .doc = Sets the incoming transition and how long it runs.

d-lzc5gbav-transition = Transition

d-lzc5gbav-seconds = Seconds

d-4vxaibys = Key at
    .doc = Sets a key of `param` at `at` (timeline seconds), at the value the field holds now: the value a curve changes from, as the inspection shows it.

d-4vxaibys-at = At

d-4vxaibys-param = Param

d-4vxaibys-interp = Interp

d-jmrgc7np = Unkey
    .doc = Removes a key.

d-jmrgc7np-k = K

d-7b5o6mn3 = Set interp
    .doc = How a key's curve leaves it.

d-7b5o6mn3-interp = Interp

d-ou2lcuwq = Sample key
    .doc = Takes the colour of the cut's frame where the person pointed, as the key that removes it.

d-ou2lcuwq-c = C

d-ou2lcuwq-at = At

d-ou2lcuwq-x = X

d-ou2lcuwq-y = Y

d-oaa6shnc = Key colour
    .doc = Sets the colour the key removes to one of the usual screens.

d-oaa6shnc-red = Red

d-oaa6shnc-green = Green

d-oaa6shnc-blue = Blue

d-5rq4qoik = Thumb frame
    .doc = One frame of a source at `at` seconds in it, kept as a filmstrip picture.

d-5rq4qoik-at = At

d-5rq4qoik-width = Width

d-5rq4qoik-height = Height

d-xqw33rc3 = Filmstrip
    .doc = Draws eight frames of the clip across its own part, kept until a trim asks for them again.

d-xqw33rc3-c = C

d-p7dk5fld = Note marker
    .doc = Writes what a mark says.

d-p7dk5fld-note = Note

d-57xowwoj = Set align
    .doc = Sets where a title's lines sit in its box.

d-57xowwoj-align = Align

d-fxlzpync = Add title
    .doc = Adds a title to the bin and places it at `at`.
    .w-title = Title

d-fxlzpync-words = Words

d-fxlzpync-at = At

d-ftp5jwbo = Add marker
    .doc = Adds a marker at `at`.
    .w-marker = Marker { $n }

d-ftp5jwbo-at = At

d-hjooda4n = Remove marker

d-hjooda4n-k = K

d-ilbzfadr = Mark in

d-ilbzfadr-at = At

d-wx6zxdj6 = Mark out

d-wx6zxdj6-at = At

d-td2tkfzd = Clear marks

d-7l3mdo7j = Set frame rate

d-7l3mdo7j-fps = Fps

d-7kef53r3 = Set frame size

d-7kef53r3-width = Width

d-7kef53r3-height = Height

d-b3pi3srw = Adopt format
    .doc = Takes its shape from a source: what "Use its format" does.

d-b3pi3srw-m = M

d-yrqdwima = Add track

d-yrqdwima-form = Form

d-bztzhcrl = Delete the track

d-bztzhcrl-t = T

d-odoaypxs = Link clips
    .doc = Makes two clips one shot's picture and sound: they move and cut together.

d-odoaypxs-a = A

d-odoaypxs-b = B

d-l5aevct6 = Unlink clip

d-m4ovyotr = Split audio
    .doc = Moves a picture clip's own sound onto an audio clip of its own, linked to it.

d-m4ovyotr-c = C

d-r7zfwmwy = Place side by side
    .doc = Puts two clips side by side: each half the frame, one left and one right.

d-r7zfwmwy-left = Left

d-r7zfwmwy-right = Right

d-jp3ic5pr = Unplace side by side

d-iry5qrqv = Add effect

d-iry5qrqv-form = Form

d-7gjywuct = Remove effect

d-7gjywuct-e = E

d-aqdtfj76 = Preview
    .doc = Draws the frame the preview shows at `at`.

d-aqdtfj76-at = At

d-jolinmlp = Seek at
    .doc = Draws the frame at the ruler's place `x`, as a drag along it asks for.

d-jolinmlp-x = X

d-jolinmlp-span = Span

d-jolinmlp-scroll = Scroll

d-dibp67ku = Preview cut
    .doc = Renders the cut small, so the window plays it.

d-mrqvrzq6 = Export cut
    .doc = Renders the cut to a file.

d-mrqvrzq6-resolution = Resolution

d-mrqvrzq6-container = Container

d-mrqvrzq6-fps = Fps

d-fdcfxbsk = Poll render
    .doc = Asks how far the running render has got.

d-5m5ch2tv = Cancel render

d-pupgh4ue = Form words
    .w-sound = Sound
    .w-still = Still
    .w-title = Title
    .w-video = Video

d-d4536bfd = Interp words
    .w-hold = Hold
    .w-ease = Ease
    .w-linear = Linear

d-kga7o3ur = Window
    .w-rendering-the-cut = Rendering the cut… { $pct }%
    .w-the-film-is-ready = { $name } — the film is ready
    .w-video-editor = Video Editor
    .w-new-project = New project
    .w-projects = Projects
    .w-sources = Sources
    .w-drop-video-sound-or-pictures = Drop video, sound or pictures
    .w-they-become-clips-of-this-cut = They become clips of this cut
    .w-title = Title
    .w-nothing-to-show-yet = Nothing to show yet
    .w-add-a-source-and-the-frame = Add a source and the frame at the playhead stands here
    .w-back-a-second = Back a second
    .w-pause = Pause
    .w-play-the-cut = Play the cut
    .w-render-and-play-the-cut = Render and play the cut
    .w-forward-a-second = Forward a second
    .w-mark-in = Mark in
    .w-mark-out = Mark out
    .w-clear = Clear
    .w-frame = Frame
    .w-what-the-cut-plays-through = What the cut plays through
    .w-rendering-a-preview = Rendering a preview
    .w-rendering-the-film = Rendering the film
    .w-stop-the-render = Stop the render
    .w-the-engine-could-not-do-that = The engine could not do that
    .w-zoom-out = Zoom out
    .w-zoom-in = Zoom in
    .w-fit = Fit
    .w-earlier = Earlier
    .w-later = Later
    .w-split = Split
    .w-delete = Delete
    .w-marker = Marker
    .w-add-a-video-track = Add a video track
    .w-add-a-sound-track = Add a sound track
    .w-the-ruler = The ruler
    .w-the-timeline = The timeline
    .w-inspector = Inspector
    .w-clip = Clip
    .w-look = Look
    .w-effects = Effects
    .w-name = Name
    .w-opacity = Opacity
    .w-volume = Volume
    .w-speed = Speed
    .w-play-it-backwards = Play it backwards
    .w-fade-in-seconds = Fade in, seconds
    .w-fade-out-seconds = Fade out, seconds
    .w-transition = Transition
    .w-cut = Cut
    .w-dissolve = Dissolve
    .w-fade-in = Fade in
    .w-dip-to-black = Dip to black
    .w-wipe = Wipe
    .w-use-its-format = Use its format
    .w-split-the-sound = Split the sound
    .w-unlink = Unlink
    .w-marker-here = Marker here
    .w-sound-level = Sound level
    .w-brightness = Brightness
    .w-contrast = Contrast
    .w-saturation = Saturation
    .w-width = Width
    .w-across = Across
    .w-up-and-down = Up and down
    .w-turn-degrees = Turn, degrees
    .w-crop-from-the-left = Crop from the left
    .w-crop-from-the-right = Crop from the right
    .w-remove-a-colour = Remove a colour
    .w-screen-colour = Screen colour
    .w-green = Green
    .w-blue = Blue
    .w-magenta = Magenta
    .w-pick-from-the-frame = Pick from the frame
    .w-drag-across-the-picture-where-the = Drag across the picture where the colour is
    .w-pick-a-colour-from-the-frame = Pick a colour from the frame
    .w-how-much-of-it = How much of it
    .w-soft-edge = Soft edge
    .w-take-the-tint-away = Take the tint away
    .w-keys = Keys
    .w-key-a-field-here = Key a field here
    .w-turn = Turn
    .w-this-clip-holds-one-value-all = This clip holds one value all the way through
    .w-linear = Linear
    .w-hold = Hold
    .w-ease = Ease
    .w-remove-the-key = Remove the key
    .w-frames = Frames
    .w-draw-the-filmstrip = Draw the filmstrip
    .w-add-an-effect = Add an effect
    .w-grayscale = Grayscale
    .w-sepia = Sepia
    .w-invert = Invert
    .w-warm = Warm
    .w-cool = Cool
    .w-vignette = Vignette
    .w-this-clip-has-no-effect-of = This clip has no effect of its own
    .w-remove = Remove { $form }
    .w-words = Words
    .w-size = Size
    .w-red = Red
    .w-bold = Bold
    .w-italic = Italic
    .w-lines = Lines
    .w-left = Left
    .w-centre = Centre
    .w-right = Right
    .w-place-at-the-playhead = Place at the playhead
    .w-choose-a-clip-or-a-source = Choose a clip or a source
    .w-markers = Markers
    .w-remove-the-mark = Remove the mark
    .w-what-it-marks = What it marks
    .w-the-cut = The cut
    .w-frames-a-second = Frames a second
    .w-height = Height
    .w-renders-from = Renders from
    .w-to = to
    .w-export = Export
    .w-mp4 = mp4
    .w-webm = webm
    .w-mov = mov
    .w-format = Format
    .w-render-the-film = Render the film
    .w-the-cut-plays-in-the-window = The cut plays in the window
    .w-save-the-film = Save the film
    .w-no-cut-yet = No cut yet
    .w-start-one-or-open-a-video = Start one, or open a video in the editor

d-bi2ut6h2 = Studio
    .w-open-in-the-editor = Open in the editor
    .w-clips = { $clips } clips
    .w-this-cut-opens-in-the-video = This cut opens in the Video Editor
    .w-its-preview-its-timeline-and-its = Its preview, its timeline and its inspector are in the app's window

d-kfetalt5 = Project row

d-kqgtcock = Media row
    .w-title = Title
