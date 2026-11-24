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

d-4wmc4l7b = Pictures
    .doc = Pictures placed in the artwork, by the address their bytes are kept at.

d-ob6rlf7s = Selection
    .doc = The area selected for the next edit, kept with the artwork.

d-k2slfiuy = Look
    .doc = The picture of the artwork as last drawn, so the surface shows paint under the hand.

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

d-erjm2yk6-filter-opacity = Opacity

d-erjm2yk6-filter-visible = Visible

d-erjm2yk6-filter-locked = Locked

d-erjm2yk6-filter-texture = Texture

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

d-cdvrgjan = Src
    .doc = A picture's bytes, by the address they are kept at, and the frame it is drawn in.

d-vniyw6t3 = X

d-v3wlgndz = Y

d-4nv5sha3 = Width

d-hlvljkif = Height

d-54j6cyar-filter-layer = Layer

d-54j6cyar-filter-size = Size

d-54j6cyar-filter-opacity = Opacity

d-54j6cyar-filter-open = Open

d-54j6cyar-filter-x = X

d-54j6cyar-filter-y = Y

d-54j6cyar-filter-width = Width

d-54j6cyar-filter-height = Height

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

d-5h6vjvk2 = Selection

d-xocfgdc7-filter-width = Width

d-xocfgdc7-filter-height = Height

d-xocfgdc7-filter-selection = Selection

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

d-tsgjljmf-filter-opacity = Opacity

d-tsgjljmf-filter-visible = Visible

d-tsgjljmf-filter-locked = Locked

d-tsgjljmf-filter-texture = Texture

d-uslws4n3 = Mark json
    .noun = { $count ->
        [one] mark json
       *[other] mark jsons
    }

d-67wvmkkq = Name

d-jk7lnwfw = Layer

d-xy2goji2 = Kind

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

d-uslws4n3-filter-layer = Layer

d-uslws4n3-filter-size = Size

d-uslws4n3-filter-opacity = Opacity

d-uslws4n3-filter-x = X

d-uslws4n3-filter-y = Y

d-uslws4n3-filter-width = Width

d-uslws4n3-filter-height = Height

d-3pgxznsb = Area json
    .noun = { $count ->
        [one] area json
       *[other] area jsons
    }

d-nxcluiyf = D

d-jxpwvxya = Feather

d-3pgxznsb-filter-feather = Feather

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

d-fbgqcv4q = Add stroke~present

d-a3gxy4s7 = Add shape~present

d-cfa5perl = Resize mark~present

d-od5n7ehr = Place picture~present

d-y2raac27 = Select area~present

d-vbu6ayw5 = Artwork
    .w-layers = Layers
    .w-hide-the-layer = Hide the layer
    .w-show-the-layer = Show the layer
    .w-layer-actions = Layer actions
    .w-opacity = Opacity
    .w-up = Up
    .w-down = Down
    .w-add-layer = Add layer
    .w-brush = Brush: { $brush }
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
    .w-fit-the-view = Fit the view
