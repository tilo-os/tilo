# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-photos = Photos

d-vf6rmvyr = Photo
    .doc = A photo in the library: its name, the address of its picture, and what the library knows of it.
    .noun = { $count ->
        [one] photo
       *[other] photos
    }
    .untitled = Photo

d-rv4vs7su = Name

d-r55t3rg3 = Picture
    .doc = Where the picture's bytes are: an address `tilo-blob://<reference>` the window's pictures read from the content store.

d-yyfm6s7t = Favorite

d-wm3tr75p = Captured at
    .doc = When the photo was taken, as the camera wrote it: kept as words until something needs to read a time from them.

d-zz3fpxbt = Folder
    .doc = The folder it came in from, where the library knows one.

d-5bhxlhcf = Tags

d-vlem53gy = People

d-bgo4zgpq = Location
    .doc = The place the photo was taken, as words a person set.

d-kko7mkgg = Latitude
    .doc = The coordinates that place names: nought where none is set.

d-4xox6lum = Longitude

d-brtcqh34 = Description
    .doc = What the picture shows, as a person or an analysis wrote it.

d-6esqywn4 = Objects
    .doc = Objects and pets the picture shows, named.

d-7rzyj24a = Pets

d-vf6rmvyr-filter-favorite = Favorite

d-vf6rmvyr-filter-tags = Tags

d-vf6rmvyr-filter-people = People

d-ur5n6nzw = Album
    .doc = A collection of photos a person named: its photos, by the ids they have in the library.
    .noun = { $count ->
        [one] album
       *[other] albums
    }
    .untitled = Album

d-unujaudz = Name

d-k7fl2y27 = Photos

d-ur5n6nzw-filter-photos = Photos

d-hc736lwy = Person
    .doc = A person the library knows: their name and the photos they are on, by id.
    .noun = { $count ->
        [one] person
       *[other] people
    }
    .untitled = Person

d-segahwtp = Name

d-vbzsnqg5 = Photos

d-hc736lwy-filter-photos = Photos

d-j45kskqf = Library
    .doc = The workspace's photo library: every photo, the albums people made of them, and the people the photos show.
    .noun = { $count ->
        [one] library
       *[other] libraries
    }

d-moeuycny = Photos

d-tqa4lyow = Albums

d-xr5mtp2m = People

d-z2bwvzat = Import photo
    .doc = Imports a photograph the person picks: the library keeps the address of its bytes and the name it came in with.
    .did = added a photo

d-z2bwvzat-file = File

d-hexbnfml = Import photo meta
    .doc = Imports a photograph a caller already knows the capture time and place of (the camera's handoff).
    .did = added a photo

d-hexbnfml-file = File

d-hexbnfml-captured_at = Captured at

d-hexbnfml-location = Location

d-vk76kjxc = Import dropped
    .doc = Imports the pictures a person drops on the window: each picture file becomes a photo, and a file that is no picture is left alone.
    .did = added photos

d-vk76kjxc-files = Files

d-4y5pwp2y = Toggle favorite
    .doc = Marks a photo as a favorite, or takes that back.

d-4y5pwp2y-favorite = Favorite

d-zyrmacyr = Favorite selected
    .doc = Marks each named photo a favorite.

d-zyrmacyr-ids = Ids

d-vmzzie5o = Remove selected
    .doc = Takes each named photo out of the library.

d-vmzzie5o-ids = Ids

d-3gje7fzu = Add to album
    .doc = Puts each named photo in an album.

d-3gje7fzu-ids = Ids

d-vr7jehtb = Tag
    .doc = Gives a photo a tag.

d-vr7jehtb-t = T

d-6asxmdwz = Untag
    .doc = Takes a tag off a photo.

d-6asxmdwz-t = T

d-uyraencp = Remove from library
    .doc = Takes a photo out of the library.

d-uyraencp-p = P

d-myakm7gv = New album
    .doc = Starts an album, and puts the photo it is started from in it.

d-myakm7gv-p = P

d-nw5ubpyd = Rename album
    .doc = Renames an album.

d-nw5ubpyd-to = To

d-jd7bl7ux = Delete album
    .doc = Deletes an album.

d-jd7bl7ux-a = A

d-nf3xznu3 = Album
    .doc = Puts a photo in an album, or takes it out of one when it is already in it.

d-nf3xznu3-p = P

d-vtt5pvr6 = Share
    .doc = The link that reaches a photo's picture: the address of its bytes, which every device of the workspace reads from the content store.

d-mexlkmwn = Add person
    .doc = Names a person on a photo, and puts the photo on that person when the library knows them.

d-mexlkmwn-p = P

d-mexlkmwn-person = Person

d-kmbs5gxw = Remove person
    .doc = Takes a person off a photo, and the photo off that person.

d-kmbs5gxw-p = P

d-kmbs5gxw-person = Person

d-fltwmykq = Rename person
    .doc = Names a person: the photos they are on say the new name too.

d-fltwmykq-person = Person

d-fltwmykq-to = To

d-fdh5jyv7 = New person
    .doc = Starts a person the library knows, with the photos it already has of them.

d-fdh5jyv7-name = Name

d-fdh5jyv7-photos = Photos

d-xy3z7l53 = Merge people
    .doc = Merges one person into another: their photos come together under the name kept, and the name merged away leaves every photo it was on.

d-xy3z7l53-from = From

d-xy3z7l53-into = Into

d-f6pmjtlz = Take off this person
    .doc = Takes one photo off a person, and the person off that photo.

d-f6pmjtlz-person = Person

d-f6pmjtlz-photo = Photo

d-s6nznpqj = Forget person
    .doc = Forgets a person: their name leaves every photo they were on.

d-s6nznpqj-person = Person

d-ihd2ld2t = Set place
    .doc = Sets the place a photo was taken from where the person points on the map: the picker reports the point as its latitude then its longitude.

d-ihd2ld2t-at = At

d-4ufdgmft = Clear place
    .doc = Takes the place off a photo.

d-i45tq7ne = Save photograph
    .doc = The original picture as a file, to save where the person says.

d-hfgcskel = Library
    .w-photos = Photos
    .w-select-all = Select all
    .w-clear-selection = Clear selection
    .w-favorite-selected = Favorite selected
    .w-remove-selected = Remove selected
    .w-done = Done
    .w-open = Open
    .w-select = Select
    .w-present = Present
    .w-library = Library
    .w-favorites = Favorites
    .w-timeline = Timeline
    .w-people = People
    .w-tags = Tags
    .w-folders = Folders
    .w-places = Places
    .w-albums = Albums
    .w-previous = Previous
    .w-next = Next
    .w-stop = Stop
    .w-zoom-out = Zoom out
    .w-zoom-in = Zoom in
    .w-actual-size = Actual size
    .w-back = Back
    .w-remove-from-favorites = Remove from favorites
    .w-favorite = Favorite
    .w-share = Share
    .w-remove = Remove
    .w-take-this-photo-out-of-the = Take this photo out of the library?
    .w-remove-from-library = Remove from library
    .w-cancel = Cancel
    .w-no-people-yet = No people yet
    .w-the-vision-service-proposes-the-people = The vision service proposes the people in your photos; name them, merge them and split them here.

d-i4ssir2r = Person row
    .w-photos = { $len } photos

d-pwxkkjpa = Person
    .w-photos = { $len } photos

d-opj2blvg = Empty place
    .w-no-favorites-yet = No favorites yet
    .w-star-a-photo-to-find-it = Star a photo to find it here.
    .w-no-people-yet = No people yet
    .w-name-the-people-on-a-photo = Name the people on a photo in its details.
    .w-no-tags-yet = No tags yet
    .w-tag-a-photo-in-its-details = Tag a photo in its details.
    .w-no-folders-yet = No folders yet
    .w-a-photo-s-folder-is-where = A photo's folder is where the library knows it came from.
    .w-no-places-yet = No places yet
    .w-give-a-photo-a-place-on = Give a photo a place on the map in its details.
    .w-this-album-is-empty = This album is empty
    .w-add-photos-to-it-from-the = Add photos to it from the library.
    .w-your-library-is-empty = Your library is empty
    .w-open-a-photo-or-drop-pictures = Open a photo, or drop pictures on the window, to start your library.

d-dy2z5spw = Details fields
    .w-captured-time = Captured time
    .w-folder = Folder
    .w-place = Place
    .w-description = Description
    .w-where-it-was-taken = Where it was taken
    .w-save-photograph = Save photograph
    .w-clear-place = Clear place

d-dho5ecc7 = Details
    .w-albums = Albums

d-coxhshbn = Photo
    .w-zoom-out = Zoom out
    .w-zoom-in = Zoom in
    .w-actual-size = Actual size
    .w-remove-from-favorites = Remove from favorites
    .w-favorite = Favorite

d-vy4hppbk = Photo row

d-5qykqlov = Photos
    .w-no-photos-yet = No photos yet
