# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-canvas = Canvas

d-so-tilo-canvas-keywords = draw, drawing, paint, sketch, whiteboard, art, illustration, canvas, vector, watercolour, oil, zeichnen, malen, skizze, zeichenbrett, leinwand, tafel

d-mwpf2yfh = Artwork
    .doc = An artwork: layers of marks, each a vector object with a stable id, over a ground.
    .noun = { $count ->
        [one] artwork
       *[other] artworks
    }
    .untitled = Untitled artwork

d-zp2ztcwq = Title

d-izpefh4x = Width

d-5ui5uh6r = Height

d-fh35sews = Ground texture
    .doc = `smooth`, `paper`, `cold-press`, `rough`, `linen` or `panel`.

d-zdv3lz52 = Ground colour

d-xu4h6ew2 = Layers
    .doc = Bottom to top.

d-btwv4xq5 = Marks
    .doc = Every mark of every layer; a mark names its layer.

d-4bht3tzh = Media
    .doc = The media presets saved in the artwork, which its layers may name.

d-4wmc4l7b = Pictures
    .doc = Pictures placed in the artwork, by the address their bytes are kept at.

d-ob6rlf7s = Selection
    .doc = The area selected for the next edit, kept with the artwork.

d-exp6h3eo = Reference
    .doc = The picture to work from, which `compare_reference` shows the artwork against; it is not part of the work and never exported.

d-k2slfiuy = Look
    .doc = The picture of the artwork as last drawn, so the surface shows paint under the hand.

d-mksntvfu = Comparison
    .doc = The artwork beside, over or against its reference as last compared, kept to look at.

d-meooqdog = Notice
    .doc = Why the last thing asked of the engine did not happen; empty when it did.

d-erjm2yk6 = Layer
    .doc = One layer of the artwork: its own paint and what it lets through.
    .noun = { $count ->
        [one] layer
       *[other] layers
    }

d-bplq2q5j = Name

d-ilxjjxxz = Medium
    .doc = `oil`, `watercolour` or another medium saved in the artwork; empty for plain vector paint.

d-smoxtxmk = Opacity

d-evne54yw = Visible

d-nagaw3zw = Locked

d-yzvcnjuy = Texture
    .doc = How much of the ground's tooth shows through its marks, 0 to 1.

d-cwenklqw = Group
    .doc = The folder the layer stands in; empty for an ungrouped layer.

d-rxz73i7f = Mask of
    .doc = The name of the layer this one masks: its marks are drawn in greys, light showing the layer and dark hiding it.

d-umkqodij = Base
    .doc = A mask's grey before any mark, 0 to 1.

d-owktdb7d = Invert

d-n43pvuyc = Clip
    .doc = Clipped to the layer below: it shows only where that layer has paint.

d-2rtbmdqf = Unlinked
    .doc = A mask that stays put when its layer's marks move.

d-erjm2yk6-filter-opacity = Opacity

d-erjm2yk6-filter-visible = Visible

d-erjm2yk6-filter-locked = Locked

d-erjm2yk6-filter-texture = Texture

d-erjm2yk6-filter-base = Base

d-erjm2yk6-filter-invert = Invert

d-erjm2yk6-filter-clip = Clip

d-erjm2yk6-filter-unlinked = Unlinked

d-54j6cyar = Mark
    .doc = One mark: a brush stroke (`stroke`) or a shape (`path`) drawn on a layer.
    .noun = { $count ->
        [one] mark
       *[other] marks
    }

d-3mgqcu4c = Name

d-xeahkwbd = Layer

d-pwxdvgoe = Kind
    .doc = `stroke` or `path`.

d-4eq3fpao = Points
    .doc = A stroke's points, `x, y, pressure` in turn, in artwork units.

d-qxx46dqp = D
    .doc = A shape's SVG path data, or the area it is kept inside.

d-vmqyvhnc = Size

d-fnl5j4dd = Colour

d-omem6tkf = Brush
    .doc = `round`, `pen`, `pencil`, `marker`, `airbrush`, `charcoal`, `calligraphy`, `eraser`, `pixel` or `pixel-eraser`; empty for the round ink brush.

d-kytiqfia = Opacity

d-y5teghkv = Open
    .doc = Whether the hand is still on it: the stroke a moving pointer extends.

d-hyumfmpg = Group
    .doc = The folder the mark stands in; empty for ungrouped.

d-lu2gjrfu = Wet
    .doc = On a painted layer: paint into what is still wet instead of over dry paint.

d-s2tjjp2p = Params
    .doc = On a painted layer: the medium's parameters overridden for this mark.

d-cdvrgjan = Src
    .doc = A picture's bytes, by the address they are kept at, and the frame it is drawn in.

d-vniyw6t3 = X

d-v3wlgndz = Y

d-4nv5sha3 = Width

d-hlvljkif = Height

d-o4zghm7q = Blend
    .doc = How the mark mixes with what is under it: empty or `normal`, `multiply`, `screen`, `overlay`, `darken`, `lighten`, `color-dodge`, `color-burn`, `hard-light`, `soft-light`, `difference` or `exclusion`.

d-wpv66rxy = Crop x
    .doc = The picture's frame inside its own pixels, as fractions of them: all of it by default.

d-sz6fzruq = Crop y

d-ax7jjgo5 = Crop width

d-7jtq2b7o = Crop height

d-mzyxo2mo = Straighten
    .doc = A turn inside the picture's frame, in degrees.

d-xndatwwg = Flip h

d-vyaxfcb2 = Flip v

d-37ailkzg = Exposure
    .doc = Colour and tone adjustments, applied when the artwork is shown and exported; zero (or off, for the flags) leaves a step out.

d-g3ifa5op = Contrast

d-3taebyjo = Highlights

d-5d6dfhop = Shadows

d-nebzyt3a = Saturation

d-6ib7oqy4 = Vibrance

d-vzagvyr3 = Hue

d-bdxzjdby = Temperature

d-skvl4xui = Tint

d-3tuoc3tu = Grayscale

d-tr7kuqc5 = Invert

d-qe7z2w5c = Blur

d-h6rqzqu5 = Sharpen

d-glowmwgx = Vignette

d-jzqxuy4d = Gradient
    .doc = A gradient fill drawn instead of the plain colour: `linear` or `radial`, its two ends and its stops.

d-2afylrq3 = Gradient x0

d-skgor2yj = Gradient y0

d-ctpi3sbl = Gradient x1

d-nosksv3s = Gradient y1

d-n4zfbtzn = Gradient offsets

d-7vai53br = Gradient colours

d-63yhsfnf = Text
    .doc = A text mark's words, its type and its setting; `kind` is `text` for one.

d-xbmipp4j = Font

d-ymnuknae = Weight

d-u62kvwpy = Italic

d-mj3rz5s2 = Align

d-i7zjtca3 = Line height

d-sc445r7q = Tracking

d-e7l24fze = Text path

d-54j6cyar-filter-layer = Layer

d-54j6cyar-filter-size = Size

d-54j6cyar-filter-opacity = Opacity

d-54j6cyar-filter-open = Open

d-54j6cyar-filter-wet = Wet

d-54j6cyar-filter-x = X

d-54j6cyar-filter-y = Y

d-54j6cyar-filter-width = Width

d-54j6cyar-filter-height = Height

d-54j6cyar-filter-crop_x = Crop x

d-54j6cyar-filter-crop_y = Crop y

d-54j6cyar-filter-crop_width = Crop width

d-54j6cyar-filter-crop_height = Crop height

d-54j6cyar-filter-straighten = Straighten

d-54j6cyar-filter-flip_h = Flip h

d-54j6cyar-filter-flip_v = Flip v

d-54j6cyar-filter-exposure = Exposure

d-54j6cyar-filter-contrast = Contrast

d-54j6cyar-filter-highlights = Highlights

d-54j6cyar-filter-shadows = Shadows

d-54j6cyar-filter-saturation = Saturation

d-54j6cyar-filter-vibrance = Vibrance

d-54j6cyar-filter-hue = Hue

d-54j6cyar-filter-temperature = Temperature

d-54j6cyar-filter-tint = Tint

d-54j6cyar-filter-grayscale = Grayscale

d-54j6cyar-filter-invert = Invert

d-54j6cyar-filter-blur = Blur

d-54j6cyar-filter-sharpen = Sharpen

d-54j6cyar-filter-vignette = Vignette

d-54j6cyar-filter-gradient_x0 = Gradient x0

d-54j6cyar-filter-gradient_y0 = Gradient y0

d-54j6cyar-filter-gradient_x1 = Gradient x1

d-54j6cyar-filter-gradient_y1 = Gradient y1

d-54j6cyar-filter-weight = Weight

d-54j6cyar-filter-italic = Italic

d-54j6cyar-filter-line_height = Line height

d-54j6cyar-filter-tracking = Tracking

d-73pl6fmd = Area
    .doc = The area selected for the next edit.
    .noun = { $count ->
        [one] area
       *[other] areas
    }

d-k26xq4t7 = D
    .doc = SVG path data in artwork units.

d-enongrrr = Feather
    .doc = How far its edge fades, in units.

d-73pl6fmd-filter-feather = Feather

d-47oq4fqf = Picture
    .doc = A picture the artwork shows, kept by the address of its bytes.
    .noun = { $count ->
        [one] picture
       *[other] pictures
    }

d-di5h6qir = Name

d-4be6ojwm = Url

d-rp5ikqj7 = Width

d-4r7lfx7t = Height

d-47oq4fqf-filter-width = Width

d-47oq4fqf-filter-height = Height

d-hxu4t2k5 = Medium
    .doc = A medium saved in the artwork: a preset of oil or watercolour the layers may be painted with.
    .noun = { $count ->
        [one] medium
       *[other] mediums
    }

d-bevph2qk = Key

d-cgytyo4j = Label

d-2opuzdzr = Engine
    .doc = `oil` or `watercolour`.

d-mgi6schv = Description

d-6r6opsjz = Ground
    .doc = The ground it suits; empty for any.

d-hids323q = Params

d-rdqoi6b4 = Palette
    .doc = The pigments it carries, loaded as `pigment:<name>`.

d-hxh5n7pe = Pigment
    .doc = A pigment a medium carries.
    .noun = { $count ->
        [one] pigment
       *[other] pigments
    }

d-ph4qfvlv = Name
    .doc = Lowercase words joined by `-`, such as `burnt-sienna`.

d-6heumgwe = Colour
    .doc = Masstone colour, `#rrggbb`.

d-tm7hljog = Granulation
    .doc = Watercolour: how much it settles into the paper's tooth, 0 to 1.

d-t2xyhkxn = Strength
    .doc = How strongly it tints, around 1.

d-hxh5n7pe-filter-granulation = Granulation

d-hxh5n7pe-filter-strength = Strength

d-yfajq2fi = Reference
    .doc = A picture to work from, kept by the address of its bytes: the artwork is compared against it.
    .noun = { $count ->
        [one] reference
       *[other] references
    }

d-3ze3qk7e = Image

d-ky6t7zk4 = X

d-k5uohe26 = Y

d-vyzji3vv = Width

d-peuuxvxb = Height

d-decytxpt = Opacity

d-j3jxn7s3 = Visible

d-yfajq2fi-filter-x = X

d-yfajq2fi-filter-y = Y

d-yfajq2fi-filter-width = Width

d-yfajq2fi-filter-height = Height

d-yfajq2fi-filter-opacity = Opacity

d-yfajq2fi-filter-visible = Visible

d-wngt6wa6 = Library
    .doc = The workspace's artworks.
    .noun = { $count ->
        [one] library
       *[other] libraries
    }

d-5gxbqk6v = Artworks

d-crj47b4n = Look request
    .doc = What a look asks the device's drawing engine for.
    .noun = { $count ->
        [one] look request
       *[other] look requests
    }

d-hhu6bz5c = Size

d-ubid4c7s = Format

d-crj47b4n-filter-size = Size

d-b3ilsucz = Export request
    .doc = What an export asks the device's drawing engine for.
    .noun = { $count ->
        [one] export request
       *[other] export requests
    }

d-ottkj6jd = Format

d-5mutx32x = Scale

d-b3ilsucz-filter-scale = Scale

d-6w6ch7ag = Svg request
    .doc = What an SVG export asks for: the whole artwork, its text as text.
    .noun = { $count ->
        [one] svg request
       *[other] svg requests
    }

d-c35hmrf6 = Ids

d-gqfdwswn = Outline text

d-6w6ch7ag-filter-outline_text = Outline text

d-2kyhrm65 = Place request
    .doc = Where an imported SVG goes: the top layer, in a group of its own.
    .noun = { $count ->
        [one] place request
       *[other] place requests
    }

d-2w4n3dr2 = Layer

d-zxu35ztu = Group

d-ulngw3pf = Fit

d-xocfgdc7 = Artwork json
    .doc = The artwork as the device's drawing engine reads it: its fields, and each mark with the layer it stands on.
    .noun = { $count ->
        [one] artwork json
       *[other] artwork jsons
    }

d-j3bzgr3f = Title

d-dowdfhi2 = Width

d-vlrgzghe = Height

d-lotrow4q = Ground texture

d-mpyvrw7z = Ground colour

d-xubd3xon = Layers

d-lowvdtkg = Marks

d-hoix2o4s = Media

d-5h6vjvk2 = Selection

d-n77pojfm = Reference

d-xocfgdc7-filter-width = Width

d-xocfgdc7-filter-height = Height

d-xocfgdc7-filter-selection = Selection

d-xocfgdc7-filter-reference = Reference

d-tsgjljmf = Layer json
    .noun = { $count ->
        [one] layer json
       *[other] layer jsons
    }

d-xunw6qmv = Name

d-sxka2uxh = Medium

d-aka7ql3b = Opacity

d-4h7cxhog = Visible

d-2r4nberb = Locked

d-xlvw3rkz = Texture

d-km2u7cyx = Group

d-qhmnxju4 = Mask of

d-6oqanygz = Base

d-y6iyulut = Invert

d-k2r2z54p = Clip

d-ppv32nn6 = Unlinked

d-tsgjljmf-filter-opacity = Opacity

d-tsgjljmf-filter-visible = Visible

d-tsgjljmf-filter-locked = Locked

d-tsgjljmf-filter-texture = Texture

d-tsgjljmf-filter-base = Base

d-tsgjljmf-filter-invert = Invert

d-tsgjljmf-filter-clip = Clip

d-tsgjljmf-filter-unlinked = Unlinked

d-xow6iril = Medium json
    .noun = { $count ->
        [one] medium json
       *[other] medium jsons
    }

d-iztu5hbq = Id

d-shen6puc = Label

d-pcwg6twm = Engine

d-tucyloxp = Description

d-3g5vtp7l = Ground

d-athamtu3 = Params

d-pq2ipocw = Palette

d-6dypgs2o = Pigment json
    .noun = { $count ->
        [one] pigment json
       *[other] pigment jsons
    }

d-dgwlsywk = Name

d-b74p3dso = Colour

d-rfus746j = Granulation

d-veeu7fpc = Strength

d-6dypgs2o-filter-granulation = Granulation

d-6dypgs2o-filter-strength = Strength

d-4d5hhlsw = Reference json
    .noun = { $count ->
        [one] reference json
       *[other] reference jsons
    }

d-go76fdwt = Image

d-ax3jntcc = Frame

d-urv4gnep = Opacity

d-zbqd3iad = Visible

d-4d5hhlsw-filter-opacity = Opacity

d-4d5hhlsw-filter-visible = Visible

d-uslws4n3 = Mark json
    .noun = { $count ->
        [one] mark json
       *[other] mark jsons
    }

d-67wvmkkq = Name

d-jk7lnwfw = Layer

d-xy2goji2 = Kind

d-lrfhvryb = Group

d-ysn6onwt = Points

d-vmvvarxw = D

d-hajhmux3 = Size

d-gtn2xdwh = Colour

d-pqrpx7k3 = Brush

d-kolk77nz = Opacity

d-7hgp7nbk = Src

d-vtafbavc = X

d-el3izuuy = Y

d-jzpg25ot = Width

d-k7jjvary = Height

d-f2yfdpsw = Blend

d-gbswtfkm = Crop x

d-cnv33zk4 = Crop y

d-yzjgjp5q = Crop width

d-d2tduo5n = Crop height

d-j7tormbq = Straighten

d-vmes3q7g = Flip h

d-4tv2wnsi = Flip v

d-tnsniv2d = Exposure

d-xsuhepir = Contrast

d-rhmkylpp = Highlights

d-qqdl7hij = Shadows

d-asuq3lgp = Saturation

d-fxjyqy3l = Vibrance

d-rf2rek22 = Hue

d-oqo6d26l = Temperature

d-suw3mw5t = Tint

d-prqekzin = Grayscale

d-ulnewmko = Invert

d-hhwbutye = Blur

d-aooc5sn5 = Sharpen

d-y32qfhyc = Vignette

d-r77f22g6 = Gradient kind

d-nuba72na = Gradient x0

d-yvaatile = Gradient y0

d-jdbhzols = Gradient x1

d-tv6gyxgx = Gradient y1

d-ih74f5za = Gradient offsets

d-zvcp7y2l = Gradient colours

d-xub3tsun = Text

d-fkp53ei3 = Font

d-7nusoc27 = Weight

d-n3izluzh = Italic

d-5dk4ceey = Align

d-gnfltu6g = Line height

d-u2tmalez = Tracking

d-hyqcnmz5 = Text path

d-q73msd2q = Wet

d-gfq7akbn = Params

d-uslws4n3-filter-layer = Layer

d-uslws4n3-filter-size = Size

d-uslws4n3-filter-opacity = Opacity

d-uslws4n3-filter-x = X

d-uslws4n3-filter-y = Y

d-uslws4n3-filter-width = Width

d-uslws4n3-filter-height = Height

d-uslws4n3-filter-crop_x = Crop x

d-uslws4n3-filter-crop_y = Crop y

d-uslws4n3-filter-crop_width = Crop width

d-uslws4n3-filter-crop_height = Crop height

d-uslws4n3-filter-straighten = Straighten

d-uslws4n3-filter-flip_h = Flip h

d-uslws4n3-filter-flip_v = Flip v

d-uslws4n3-filter-exposure = Exposure

d-uslws4n3-filter-contrast = Contrast

d-uslws4n3-filter-highlights = Highlights

d-uslws4n3-filter-shadows = Shadows

d-uslws4n3-filter-saturation = Saturation

d-uslws4n3-filter-vibrance = Vibrance

d-uslws4n3-filter-hue = Hue

d-uslws4n3-filter-temperature = Temperature

d-uslws4n3-filter-tint = Tint

d-uslws4n3-filter-grayscale = Grayscale

d-uslws4n3-filter-invert = Invert

d-uslws4n3-filter-blur = Blur

d-uslws4n3-filter-sharpen = Sharpen

d-uslws4n3-filter-vignette = Vignette

d-uslws4n3-filter-gradient_x0 = Gradient x0

d-uslws4n3-filter-gradient_y0 = Gradient y0

d-uslws4n3-filter-gradient_x1 = Gradient x1

d-uslws4n3-filter-gradient_y1 = Gradient y1

d-uslws4n3-filter-weight = Weight

d-uslws4n3-filter-italic = Italic

d-uslws4n3-filter-line_height = Line height

d-uslws4n3-filter-tracking = Tracking

d-uslws4n3-filter-wet = Wet

d-3pgxznsb = Area json
    .noun = { $count ->
        [one] area json
       *[other] area jsons
    }

d-nxcluiyf = D

d-jxpwvxya = Feather

d-3pgxznsb-filter-feather = Feather

d-ueoeipix = Grade ask
    .doc = What a picture's grade asks the device's drawing engine for.
    .noun = { $count ->
        [one] grade ask
       *[other] grade asks
    }

d-fy7u7ntb = Exposure

d-h7ykiuc5 = Contrast

d-akjbo2e5 = Highlights

d-gx2o2imz = Shadows

d-gwfjuivz = Saturation

d-y7wzyyqq = Vibrance

d-pkwjhrnw = Hue

d-gvwo4klz = Temperature

d-6bogm2iw = Tint

d-mz4xapep = Grayscale

d-7pnhizf2 = Invert

d-vzbhth5d = Blur

d-ayg5wxga = Sharpen

d-bporvvfd = Vignette

d-ueoeipix-filter-exposure = Exposure

d-ueoeipix-filter-contrast = Contrast

d-ueoeipix-filter-highlights = Highlights

d-ueoeipix-filter-shadows = Shadows

d-ueoeipix-filter-saturation = Saturation

d-ueoeipix-filter-vibrance = Vibrance

d-ueoeipix-filter-hue = Hue

d-ueoeipix-filter-temperature = Temperature

d-ueoeipix-filter-tint = Tint

d-ueoeipix-filter-grayscale = Grayscale

d-ueoeipix-filter-invert = Invert

d-ueoeipix-filter-blur = Blur

d-ueoeipix-filter-sharpen = Sharpen

d-ueoeipix-filter-vignette = Vignette

d-h5crbxpj = Wand ask
    .doc = What a magic wand asks the device's drawing engine for: the area like a colour the person pointed at, answered in the artwork's units through the picture's frame.
    .noun = { $count ->
        [one] wand ask
       *[other] wand asks
    }

d-yyqmyjfc = At

d-etjvwbve = Colour

d-giesycja = Tolerance

d-vry7txyu = Everywhere

d-e3v56jsz = Frame

d-h5crbxpj-filter-tolerance = Tolerance

d-h5crbxpj-filter-everywhere = Everywhere

d-2vbtmxov = Find ask
    .doc = What a subject or a refined edge asks the device's drawing engine for.
    .noun = { $count ->
        [one] find ask
       *[other] find asks
    }

d-3ug7cc6p = Subject

d-mgf542kb = Refine

d-oehqnmnq = At

d-t76lt6eq = Colour

d-rk2wxm5k = Tolerance

d-ipz3kxni = Layer

d-5ijkh76y = Feather

d-2vbtmxov-filter-subject = Subject

d-2vbtmxov-filter-refine = Refine

d-2vbtmxov-filter-tolerance = Tolerance

d-2vbtmxov-filter-feather = Feather

d-pa6r6tbu = Combine ask
    .doc = What a combine asks the device's drawing engine for.
    .noun = { $count ->
        [one] combine ask
       *[other] combine asks
    }

d-bp3zj5os = Ids

d-4kf2vu7l = Op

d-uc64pzzc = Measure ask
    .doc = What a measurement asks for: the marks or groups it weighs.
    .noun = { $count ->
        [one] measure ask
       *[other] measure asks
    }

d-2tk6phpq = Ids

d-ebhm4qyp = Compare ask
    .doc = What a comparison with the reference asks for.
    .noun = { $count ->
        [one] compare ask
       *[other] compare asks
    }

d-mztro4wb = Region

d-67462gfa = Mode

d-hbu4rn3a = Size

d-ebhm4qyp-filter-size = Size

d-ywazcrrf = Open artwork
    .doc = Opens a saved artwork from the file a person, Files or another app hands over.
    .did = opened an artwork

d-ywazcrrf-file = File

d-6b4cocc6 = Blank with
    .doc = A blank artwork begun with a problem to show, so a file that cannot be opened still lands where a person can read why.

d-6b4cocc6-why = Why

d-i2shd26j = New artwork
    .doc = Starts a blank artwork with one plain layer.
    .did = started a new artwork
    .w-an-artwork-is-16-to-8192 = An artwork is 16 to 8192 units wide
    .w-an-artwork-is-16-to-8192-2 = An artwork is 16 to 8192 units high

d-i2shd26j-title = Title

d-i2shd26j-width = Width

d-i2shd26j-height = Height

d-im6e3ij3 = Add a layer
    .doc = Adds a layer on top of the artwork.
    .did = added a layer
    .w-a-layer-named-is-already-here = A layer named { $trim } is already here

d-im6e3ij3-name = Name

d-25k3qffg = Rename a layer
    .doc = Renames a layer.
    .did = renamed a layer
    .w-a-layer-has-a-name = A layer has a name
    .w-another-layer-is-named = Another layer is named { $trim }

d-25k3qffg-layer = Layer

d-25k3qffg-name = Name

d-dx7xxp6w = Lock a layer
    .doc = Holds a layer still, so the pointer passes through its marks, or lets its marks be picked again.
    .did = locked a layer

d-dx7xxp6w-layer = Layer

d-dx7xxp6w-locked = Locked

d-ulsonnwl = Put the layer in a folder
    .doc = Puts a layer in the folder `group`, or takes it out when that is empty.
    .did = moved a layer to a folder

d-ulsonnwl-group = Group

d-ny524tm4 = Clip the layer
    .doc = Holds a layer to the paint of the one under it, or lets it stand free.
    .did = clipped a layer

d-ny524tm4-layer = Layer

d-ny524tm4-clip = Clip

d-yjzlh5dt = Add a mask
    .doc = Adds a mask to a layer: a layer of greys the layer is seen through.
    .did = added a mask
    .w-a-mask-is-made-for-a = A mask is made for a layer, not for a mask

d-yjzlh5dt-layer = Layer

d-ss6hhi4l = Set the mask
    .doc = Sets a mask's grey before any mark, whether it hides where it is light, and whether it stays put when its layer's marks move.
    .did = set a mask
    .w-only-a-mask-is-set-this = Only a mask is set this way
    .w-a-mask-s-grey-is-0 = A mask's grey is 0 to 1

d-ss6hhi4l-layer = Layer

d-ss6hhi4l-base = Base

d-ss6hhi4l-invert = Invert

d-ss6hhi4l-unlinked = Unlinked

d-usk4we7m = Put the mark in a folder
    .doc = Puts a mark in the folder `group`, or takes it out when that is empty.
    .did = moved a mark to a folder

d-usk4we7m-group = Group

d-mqefo2tn = Add text
    .doc = Adds a text mark: words set in the artwork's own fonts, at `x`, `y` (its first baseline) in artwork units.
    .did = added text
    .w-there-is-no-text-to-add = There is no text to add
    .w-text-is-larger-than-nothing = Text is larger than nothing
    .w-a-weight-is-100-to-900 = A weight is 100 to 900

d-mqefo2tn-layer = Layer

d-mqefo2tn-text = Text

d-mqefo2tn-x = X

d-mqefo2tn-y = Y

d-mqefo2tn-size = Size

d-mqefo2tn-colour = Colour

d-mqefo2tn-font = Font

d-mqefo2tn-weight = Weight

d-mqefo2tn-italic = Italic

d-mqefo2tn-align = Align

d-mqefo2tn-line_height = Line height

d-mqefo2tn-tracking = Tracking

d-mqefo2tn-path = Path

d-pirwd2l5 = Fill with a gradient
    .doc = Fills a mark with a gradient from `from` to `to`, its stops given as offsets 0 to 1 beside their colours.
    .did = filled a mark with a gradient
    .w-a-gradient-is-linear-or-radial = A gradient is linear or radial
    .w-a-gradient-has-two-or-more = A gradient has two or more stops, each with its colour

d-pirwd2l5-kind = Kind

d-pirwd2l5-x0 = X0

d-pirwd2l5-y0 = Y0

d-pirwd2l5-x1 = X1

d-pirwd2l5-y1 = Y1

d-pirwd2l5-offsets = Offsets

d-pirwd2l5-colours = Colours

d-tlxhhcet = Paint the layer with a medium
    .doc = Paints a layer with a medium: `oil`, `watercolour`, a preset saved in the artwork by its id, or empty for plain vector paint.
    .did = changed a layer's medium
    .w-a-medium-is-oil-watercolour-or = A medium is oil, watercolour, or one saved in the artwork
    .w-a-mask-is-painted-in-greys = A mask is painted in greys, with no medium

d-tlxhhcet-layer = Layer

d-tlxhhcet-medium = Medium

d-4fnb5pj3 = Save a medium
    .doc = Saves a medium in the artwork, so its layers name it and every device paints it the same.
    .did = saved a medium
    .w-a-medium-has-an-id = A medium has an id
    .w-a-medium-is-oil-or-watercolour = A medium is oil or watercolour

d-4fnb5pj3-id = Id

d-4fnb5pj3-label = Label

d-4fnb5pj3-engine = Engine

d-4fnb5pj3-description = Description

d-4fnb5pj3-ground = Ground

d-4fnb5pj3-params = Params

d-4fnb5pj3-palette = Palette

d-keg6cder = Remove a medium

d-keg6cder-m = M

d-icmpbgfp = Paint into wet
    .doc = Paints a mark into what is still wet, or over dry paint.
    .did = changed how a mark meets the wet paint

d-icmpbgfp-wet = Wet

d-pwpqgbcc = Clear the gradient
    .doc = Clears a mark's gradient, leaving the plain colour.

d-bzc334mn = Set the text
    .doc = Sets a text mark's words, its type and its setting.
    .did = changed text
    .w-only-a-text-mark-is-set = Only a text mark is set this way
    .w-text-is-larger-than-nothing = Text is larger than nothing

d-bzc334mn-text = Text

d-bzc334mn-size = Size

d-bzc334mn-colour = Colour

d-bzc334mn-font = Font

d-bzc334mn-weight = Weight

d-bzc334mn-italic = Italic

d-bzc334mn-align = Align

d-bzc334mn-line_height = Line height

d-bzc334mn-tracking = Tracking

d-bzc334mn-path = Path

d-nsot2laz = Move a layer up
    .doc = Moves a layer one place up in the stack.
    .did = moved a layer

d-nsot2laz-layer = Layer

d-36qh4o3t = Move a layer down
    .doc = Moves a layer one place down in the stack.
    .did = moved a layer

d-36qh4o3t-layer = Layer

d-hgluvpdm = Duplicate a layer
    .doc = Copies a layer and everything on it, above the original.
    .did = duplicated a layer

d-hgluvpdm-layer = Layer

d-cpguer7m = Merge a layer down
    .doc = Folds a layer into the one under it: its marks join that layer, and it is gone.
    .did = merged a layer

d-cpguer7m-layer = Layer

d-26tef7a6 = Show or hide a layer
    .doc = Shows or hides a layer.
    .did = hid or showed a layer

d-26tef7a6-layer = Layer

d-26tef7a6-visible = Visible

d-g6eyi6x5 = Remove a layer

d-g6eyi6x5-layer = Layer

d-ws4qckds = Set the artwork
    .doc = Sets what the artwork is called and how big it is.
    .did = changed the artwork

d-ws4qckds-title = Title

d-ws4qckds-width = Width

d-ws4qckds-height = Height

d-nmal4fcc = Paint
    .doc = Lays one brush stroke where the hand went, its point `[x, y, pressure]` in artwork units.
    .did = painted a stroke
    .w-a-stroke-point-is-x-and = A stroke point is x and y
    .w-the-layer-is-locked = The layer is locked

d-nmal4fcc-layer = Layer

d-nmal4fcc-point = Point

d-nmal4fcc-size = Size

d-nmal4fcc-colour = Colour

d-nmal4fcc-brush = Brush

d-kqbnpkk2 = Paint
    .doc = Adds the next point to the stroke being painted.
    .did = painted a stroke

d-kqbnpkk2-m = M

d-kqbnpkk2-point = Point

d-yudcrjy3 = Top layer
    .doc = The layer a stroke lands on when none is chosen: the top one that shows and is not held still.

d-kzqsfl7r = Draw a shape
    .doc = Draws a shape, its SVG path data in artwork units.
    .did = drew a shape
    .w-a-shape-has-an-outline = A shape has an outline
    .w-the-layer-is-locked = The layer is locked

d-kzqsfl7r-layer = Layer

d-kzqsfl7r-d = D

d-kzqsfl7r-colour = Colour

d-yl33wzft = Remove a mark

d-yl33wzft-m = M

d-nv63qcbm = Change a mark's colour
    .doc = Recolours a mark.
    .did = recoloured a mark

d-nv63qcbm-m = M

d-nv63qcbm-colour = Colour

d-srya3bgt = Size a mark
    .doc = Makes a mark bigger or smaller.
    .did = sized a mark
    .w-a-mark-s-brush-is-wider = A mark's brush is wider than nothing

d-srya3bgt-m = M

d-srya3bgt-size = Size

d-ynku6xaf = Place a picture
    .doc = Places a picture the person chose on the top layer, fitted inside the artwork and centred: the picture stays its own object, so it moves, crops and syncs like paint.
    .did = placed a picture
    .w-a-picture-is-placed-from-a = A picture is placed from a file

d-ynku6xaf-file = File

d-oov7ulkw = Crop the picture
    .doc = Shows part of the picture: the frame `x`, `y`, `width`, `height` of its own pixels, each a fraction of them.
    .did = cropped a picture
    .w-only-a-picture-is-cropped = Only a picture is cropped
    .w-a-crop-starts-inside-the-picture = A crop starts inside the picture
    .w-a-crop-has-a-width-and = A crop has a width and a height
    .w-a-crop-stays-inside-the-picture = A crop stays inside the picture

d-oov7ulkw-x = X

d-oov7ulkw-y = Y

d-oov7ulkw-width = Width

d-oov7ulkw-height = Height

d-52t4qnah = Straighten the picture
    .doc = Turns the picture inside its frame, in degrees, enlarging it so no corner shows.
    .did = straightened a picture
    .w-only-a-picture-is-straightened = Only a picture is straightened
    .w-a-turn-is-180-to-180 = A turn is -180 to 180 degrees

d-52t4qnah-degrees = Degrees

d-gjkukzs4 = Flip the picture
    .doc = Mirrors the picture across its middle.
    .did = flipped a picture
    .w-only-a-picture-is-flipped = Only a picture is flipped

d-gjkukzs4-horizontal = Horizontal

d-gjkukzs4-vertical = Vertical

d-gjkukzs4-flip_h = Flip h

d-gjkukzs4-flip_v = Flip v

d-apzuceoc = Change the blend
    .doc = Sets how a mark mixes with what is under it.
    .did = changed a mark's blend

d-apzuceoc-blend = Blend

d-2m4c5qhb = Adjust the mark
    .doc = Grades a mark without touching its pixels: exposure, contrast and the rest are kept on the mark and applied wherever the artwork is shown or exported.
    .did = adjusted a mark
    .w-exposure-is-5-to-5-stops = Exposure is -5 to 5 stops
    .w-hue-is-180-to-180-degrees = Hue is -180 to 180 degrees
    .w-a-blur-is-0-to-200 = A blur is 0 to 200 units
    .w-a-sharpen-is-0-to-1 = A sharpen is 0 to 1

d-2m4c5qhb-exposure = Exposure

d-2m4c5qhb-contrast = Contrast

d-2m4c5qhb-highlights = Highlights

d-2m4c5qhb-shadows = Shadows

d-2m4c5qhb-saturation = Saturation

d-2m4c5qhb-vibrance = Vibrance

d-2m4c5qhb-hue = Hue

d-2m4c5qhb-temperature = Temperature

d-2m4c5qhb-tint = Tint

d-2m4c5qhb-grayscale = Grayscale

d-2m4c5qhb-invert = Invert

d-2m4c5qhb-blur = Blur

d-2m4c5qhb-sharpen = Sharpen

d-2m4c5qhb-vignette = Vignette

d-nfuxdnmq = Grade a picture
    .doc = A picture with those steps applied to its pixels, as a new file: what a person or an agent asks for when the graded picture itself is wanted, rather than the artwork that shows it.

d-nfuxdnmq-file = File

d-nfuxdnmq-exposure = Exposure

d-nfuxdnmq-contrast = Contrast

d-nfuxdnmq-highlights = Highlights

d-nfuxdnmq-shadows = Shadows

d-nfuxdnmq-saturation = Saturation

d-nfuxdnmq-vibrance = Vibrance

d-nfuxdnmq-hue = Hue

d-nfuxdnmq-temperature = Temperature

d-nfuxdnmq-tint = Tint

d-nfuxdnmq-grayscale = Grayscale

d-nfuxdnmq-invert = Invert

d-nfuxdnmq-blur = Blur

d-nfuxdnmq-sharpen = Sharpen

d-nfuxdnmq-vignette = Vignette

d-mm6p34kb = Select what a colour touches
    .doc = Selects what the colour at `at` touches in the artwork's picture, or that colour everywhere: the magic wand.
    .w-point-at-a-colour-or-name = Point at a colour, or name one
    .w-a-tolerance-is-0-to-1 = A tolerance is 0 to 1

d-mm6p34kb-at = At

d-mm6p34kb-colour = Colour

d-mm6p34kb-tolerance = Tolerance

d-mm6p34kb-everywhere = Everywhere

d-cbwhrf4z = Select the subject
    .doc = Selects the picture's subject, found by the on-device model the person set up, with its edge fitted to the picture's own edges over `refine` units where that is asked.
    .w-a-refine-is-0-to-200 = A refine is 0 to 200 units

d-cbwhrf4z-refine = Refine

d-le5d3j32 = Refine the selection
    .doc = Refines the selection's edge to the picture's own edges over `refine` units, keeping fur and hair soft.
    .w-select-an-area-first = Select an area first
    .w-a-refine-is-over-0-and = A refine is over 0 and at most 200 units

d-le5d3j32-refine = Refine

d-mt2l35ii = Combine shapes
    .doc = Combines the marks into one path, in the bottom one's place and colour: shapes united, subtracted, intersected or excluded.
    .did = combined shapes
    .w-name-two-or-more-marks-to = Name two or more marks to combine
    .w-shapes-unite-subtract-intersect-or-exclude = Shapes unite, subtract, intersect or exclude

d-mt2l35ii-marks = Marks

d-mt2l35ii-op = Op

d-e3ojd75d = Measure ink
    .doc = Weighs the marks' ink as the eye sees it: each one's ink box, ink area, centre of mass and its offset from the box's middle, so two icons balance when their areas and offsets match.
    .w-name-the-marks-to-measure = Name the marks to measure

d-e3ojd75d-marks = Marks

d-pe33b4yd = Set the reference
    .doc = Sets the picture to work from, fitted inside the artwork and centred.
    .did = changed the reference picture
    .w-a-reference-is-a-picture-from = A reference is a picture from a file
    .w-a-reference-is-0-to-1 = A reference is 0 to 1 opaque

d-pe33b4yd-file = File

d-pe33b4yd-opacity = Opacity

d-5n6xpa5g = Clear the reference
    .doc = Takes the reference picture away.
    .did = cleared the reference picture

d-ioblnef6 = Compare with the reference
    .doc = Shows the artwork beside (`side-by-side`), over (`overlay`) or against (`difference`) the reference picture, as a new file, with how far apart they are.
    .w-set-a-reference-picture-first = Set a reference picture first
    .w-a-comparison-is-side-by-side = A comparison is side-by-side, overlay or difference
    .w-a-comparison-is-64-to-1024 = A comparison is 64 to 1024 pixels a side

d-ioblnef6-mode = Mode

d-ioblnef6-size = Size

d-5zqnfec7 = Select an area
    .doc = Selects an area, its outline as SVG path data.
    .did = selected an area
    .w-a-selection-has-an-outline = A selection has an outline
    .w-a-feather-is-0-to-500 = A feather is 0 to 500 units

d-5zqnfec7-d = D

d-5zqnfec7-feather = Feather

d-im6ajcey = Deselect
    .doc = Lets go of the selection.

d-x4bmanto = Fill the selection
    .doc = Fills the selection as a path mark on a layer.
    .did = filled the selection

d-x4bmanto-layer = Layer

d-x4bmanto-colour = Colour

d-trlv6xc5 = Look at the artwork
    .doc = The picture of the artwork as it stands, kept on it, so the surface shows it under the hand.

d-trlv6xc5-size = Size

d-t4ze7q3o = Export a picture
    .doc = Writes the artwork at full size as a PNG, JPEG or WebP picture.
    .did = exported a picture

d-t4ze7q3o-format = Format

d-t4ze7q3o-scale = Scale

d-zjw2al2m = Export SVG
    .doc = Writes the artwork as an SVG document.
    .did = exported an SVG

d-pnm5r4tq = Save the artwork
    .doc = Saves the artwork as a `.artwork` package carrying its pictures.
    .did = saved the artwork

d-f6v2tn6o = Import an SVG
    .doc = Imports an SVG document as marks on a layer: each shape the device's drawing engine read from it becomes a mark, so the import is drawn, saved and shown like paint the person laid.
    .did = imported an SVG

d-f6v2tn6o-markup = Markup

d-qnk3hupn = Clear notice
    .doc = Clears the notice a person has read.

d-dwmbuma7 = Start paint
    .doc = Starts what the hand lays down at `point`: a stroke with the paint loaded, or nothing when the tool draws a shape, whose outline only exists once the hand lifts.

d-dwmbuma7-tool = Tool

d-dwmbuma7-brush = Brush

d-dwmbuma7-layer = Layer

d-dwmbuma7-point = Point

d-dwmbuma7-size = Size

d-dwmbuma7-colour = Colour

d-fbdnxnze = Continue paint
    .doc = Takes the next point of the stroke being painted.

d-fbdnxnze-tool = Tool

d-fbdnxnze-brush = Brush

d-fbdnxnze-point = Point

d-fbdnxnze-size = Size

d-fbdnxnze-colour = Colour

d-3nneq3xo = Finish paint
    .doc = Ends what the hand laid down at `point`: the stroke takes its last point and closes, or a shape is drawn across the drag that made it.

d-3nneq3xo-tool = Tool

d-3nneq3xo-brush = Brush

d-3nneq3xo-layer = Layer

d-3nneq3xo-from = From

d-3nneq3xo-point = Point

d-3nneq3xo-size = Size

d-3nneq3xo-colour = Colour

d-ocgibcwg = Library
    .w-new-artwork = New artwork
    .w-open-an-artwork = Open an artwork…
    .w-no-artwork-yet = No artwork yet
    .w-new-artwork-starts-one = New artwork starts one

d-sqei2kjp = New artwork~present

d-qjgxagpq = Add layer~present

d-lb23uu53 = Rename layer~present

d-zci3ltpl = Add mask~present

d-ipba7qqv = Set mask~present

d-j5cxvpsm = Add text~present

d-7lkzxbj5 = Fill gradient~present

d-jhedsahx = Set medium~present

d-zu7qeydz = Save medium~present

d-6wdxdo5a = Set text~present

d-fbgqcv4q = Add stroke~present

d-a3gxy4s7 = Add shape~present

d-cfa5perl = Resize mark~present

d-od5n7ehr = Place picture~present

d-667f442p = Crop picture~present

d-jl3at4ci = Straighten picture~present

d-id3zg3tv = Flip picture~present

d-qdptb2o7 = Adjust mark~present

d-s64swn5k = Select like~present

d-od43nccq = Select subject~present

d-o4yvpii6 = Refine selection~present

d-d3vnhf3u = Combine marks~present

d-m7kwzjqw = Measure marks~present

d-uszm3wfi = Set reference~present

d-pku6nuhg = Compare reference~present

d-y2raac27 = Select area~present

d-vbu6ayw5 = Artwork
    .w-brush = Brush
    .w-eraser = Eraser
    .w-rectangle = Rectangle
    .w-ellipse = Ellipse
    .w-line = Line
    .w-select = Select
    .w-wand = Wand
    .w-text = Text
    .w-zoom-out = Zoom out
    .w-zoom-in = Zoom in
    .w-fit = Fit
    .w-new-artwork = New artwork
    .w-add-layer = Add layer
    .w-place-a-picture = Place a picture
    .w-set-the-reference = Set the reference
    .w-look = Look
    .w-save = Save
    .w-layers = Layers
    .w-hide-the-layer = Hide the layer
    .w-show-the-layer = Show the layer
    .w-layer-actions = Layer actions
    .w-opacity = Opacity
    .w-up = Up
    .w-down = Down
    .w-medium = Medium: { $medium_said }
    .w-plain-paint = Plain paint
    .w-oil = Oil
    .w-watercolour = Watercolour
    .w-combine = Combine
    .w-unite = Unite
    .w-subtract = Subtract
    .w-intersect = Intersect
    .w-exclude = Exclude
    .w-measure = Measure
    .w-saved-media = Saved media
    .w-remove = Remove { $label }
    .w-brush-2 = Brush: { $brush }
    .w-size = Size
    .w-paint = Paint
    .w-fill-the-selection = Fill the selection
    .w-deselect = Deselect
    .w-export = Export
    .w-png-full-size = PNG, full size
    .w-png-twice-the-size = PNG, twice the size
    .w-jpeg-full-size = JPEG, full size
    .w-webp-full-size = WebP, full size
    .w-svg = SVG
    .w-gradient = Gradient
    .w-linear-into-the-paint = Linear, into the paint
    .w-radial-from-the-centre = Radial, from the centre
    .w-clear-the-gradient = Clear the gradient
    .w-reference = Reference
    .w-side-by-side = Side by side
    .w-overlay = Overlay
    .w-difference = Difference
    .w-clear-the-reference = Clear the reference
    .w-the-artwork-against-its-reference = The artwork against its reference
    .w-fit-the-view = Fit the view

d-hs75q37a = Text panel
    .w-text = Text
    .w-size = Size
    .w-text-colour = Text colour
    .w-light = Light
    .w-regular = Regular
    .w-bold = Bold
    .w-upright = Upright
    .w-italic = Italic

d-x75ur66o = Picture panel
    .w-picture = Picture
    .w-flip-across = Flip across
    .w-flip-down = Flip down
    .w-crop = Crop
    .w-whole-picture = Whole picture
    .w-centre-half = Centre half
    .w-centre-strip = Centre strip
    .w-blend = Blend: { $blend_of }
    .w-turn = Turn
    .w-exposure = Exposure
    .w-contrast = Contrast
    .w-highlights = Highlights
    .w-shadows = Shadows
    .w-saturation = Saturation
    .w-vibrance = Vibrance
    .w-hue = Hue
    .w-warmth = Warmth
    .w-tint = Tint
    .w-blur = Blur
    .w-sharpen = Sharpen
    .w-vignette = Vignette
    .w-colour = Colour
    .w-black-and-white = Black and white
    .w-un-invert = Un-invert
    .w-invert = Invert
