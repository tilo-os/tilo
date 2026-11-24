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

d-54j6cyar-filter-layer = Layer

d-54j6cyar-filter-size = Size

d-54j6cyar-filter-opacity = Opacity

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

d-akflqlvk = Opened artwork
    .doc = An artwork a `.artwork` file opened into: the fields this app keeps.
    .noun = { $count ->
        [one] opened artwork
       *[other] opened artworks
    }

d-r5usrdnc = Title

d-yogkal7v = Width

d-7y7zrgxh = Height

d-osqzkwh6 = Layers

d-akflqlvk-filter-width = Width

d-akflqlvk-filter-height = Height

d-4rfh6alh = Opened layer
    .noun = { $count ->
        [one] opened layer
       *[other] opened layers
    }

d-qvppsycy = Name

d-6ksjlnwm = Medium

d-ocg7lpks = Opacity

d-7eyz4jyf = Visible

d-4rfh6alh-filter-opacity = Opacity

d-4rfh6alh-filter-visible = Visible

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

d-im6e3ij3-name = Name

d-25k3qffg = Rename a layer
    .doc = Renames a layer.
    .did = renamed a layer
    .w-a-layer-has-a-name = A layer has a name

d-25k3qffg-layer = Layer

d-25k3qffg-name = Name

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
    .doc = The layer a stroke lands on when none is chosen: the top one that shows.

d-kzqsfl7r = Draw a shape
    .doc = Draws a shape, its SVG path data in artwork units.
    .did = drew a shape
    .w-a-shape-has-an-outline = A shape has an outline

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
    .doc = Imports an SVG document as marks on a layer.
    .did = imported an SVG

d-f6v2tn6o-markup = Markup

d-qnk3hupn = Clear notice
    .doc = Clears the notice a person has read.

d-3yu27udg = Paint point
    .doc = Paints the point the hand is at: the stroke being painted takes the next point, else a stroke starts here.

d-3yu27udg-point = Point

d-3yu27udg-size = Size

d-3yu27udg-colour = Colour

d-3yu27udg-brush = Brush

d-ocgibcwg = Library
    .w-no-artwork-yet = No artwork yet
    .w-new-artwork-starts-one = New artwork starts one

d-sqei2kjp = New artwork~present

d-lb23uu53 = Rename layer~present

d-fbgqcv4q = Add stroke~present

d-a3gxy4s7 = Add shape~present

d-cfa5perl = Resize mark~present

d-y2raac27 = Select area~present

d-vbu6ayw5 = Artwork
    .w-layers = Layers
    .w-add-layer = Add layer
    .w-size = Size
    .w-paint = Paint
