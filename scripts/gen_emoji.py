#!/usr/bin/env python3
"""Generate ui/src/emoji.rs — the shared emoji dataset for the EmojiPicker
widget and the opt-in `:shortcode:` completion on TextInput.

Table rows: (char, shortcode, [keywords]). Grouped by category.
Shortcodes must be unique (asserted below)."""

import sys

# group -> (label, tab_glyph, [ (char, shortcode, [keywords]) ])
DATA = {
    "Smileys": ("Smileys", "\U0001F600", [
        ("\U0001F600", "grinning", ["happy", "smile"]),
        ("\U0001F603", "smiley", ["happy", "joy"]),
        ("\U0001F604", "smile", ["happy", "laugh"]),
        ("\U0001F601", "grin", ["happy"]),
        ("\U0001F606", "laughing", ["haha", "lol"]),
        ("\U0001F605", "sweat_smile", ["nervous"]),
        ("\U0001F923", "rofl", ["rolling", "laugh", "lol"]),
        ("\U0001F602", "joy", ["laugh", "cry", "tears", "lol"]),
        ("\U0001F642", "slight_smile", ["smile"]),
        ("\U0001F643", "upside_down", ["silly"]),
        ("\U0001F609", "wink", ["flirt"]),
        ("\U0001F60A", "blush", ["happy", "shy"]),
        ("\U0001F607", "innocent", ["angel", "halo"]),
        ("\U0001F970", "smiling_face_with_hearts", ["love", "adore"]),
        ("\U0001F60D", "heart_eyes", ["love", "crush"]),
        ("\U0001F929", "star_struck", ["wow", "excited"]),
        ("\U0001F618", "kissing_heart", ["kiss", "love"]),
        ("\U0001F61A", "kissing_closed_eyes", ["kiss"]),
        ("\U0001F60B", "yum", ["tasty", "delicious"]),
        ("\U0001F61C", "stuck_out_tongue_wink", ["playful", "joke"]),
        ("\U0001F92A", "zany", ["goofy", "silly"]),
        ("\U0001F911", "money_mouth", ["rich", "dollar"]),
        ("\U0001F917", "hugs", ["hug"]),
        ("\U0001F92D", "hand_over_mouth", ["oops", "giggle"]),
        ("\U0001F92B", "shushing", ["quiet", "secret"]),
        ("\U0001F914", "thinking", ["hmm"]),
        ("\U0001F610", "neutral", ["meh"]),
        ("\U0001F611", "expressionless", ["blank"]),
        ("\U0001F636", "no_mouth", ["silent"]),
        ("\U0001F60F", "smirk", ["smug"]),
        ("\U0001F612", "unamused", ["meh", "annoyed"]),
        ("\U0001F644", "roll_eyes", ["annoyed", "whatever"]),
        ("\U0001F62C", "grimacing", ["awkward"]),
        ("\U0001F614", "pensive", ["sad"]),
        ("\U0001F634", "sleeping", ["zzz", "tired"]),
        ("\U0001F60C", "relieved", ["calm"]),
        ("\U0001F637", "mask", ["sick", "ill"]),
        ("\U0001F912", "thermometer_face", ["sick", "fever"]),
        ("\U0001F922", "nauseated", ["sick", "gross"]),
        ("\U0001F92E", "vomiting", ["sick", "puke"]),
        ("\U0001F927", "sneezing", ["sick", "tissue"]),
        ("\U0001F975", "hot_face", ["heat", "sweaty"]),
        ("\U0001F976", "cold_face", ["freezing"]),
        ("\U0001F635", "dizzy_face", ["woozy"]),
        ("\U0001F92F", "exploding_head", ["mind", "blown"]),
        ("\U0001F920", "cowboy", ["hat"]),
        ("\U0001F973", "partying", ["party", "celebrate"]),
        ("\U0001F60E", "sunglasses", ["cool"]),
        ("\U0001F913", "nerd", ["glasses", "geek"]),
        ("\U0001F9D0", "monocle", ["inspect"]),
        ("\U0001F615", "confused", ["unsure"]),
        ("\U0001F61F", "worried", ["concerned"]),
        ("\U0001F641", "slight_frown", ["sad"]),
        ("\U0001F62E", "open_mouth", ["wow", "surprise"]),
        ("\U0001F632", "astonished", ["shocked"]),
        ("\U0001F633", "flushed", ["embarrassed", "blush"]),
        ("\U0001F97A", "pleading", ["puppy", "eyes", "beg"]),
        ("\U0001F628", "fearful", ["scared"]),
        ("\U0001F630", "anxious", ["worried", "sweat"]),
        ("\U0001F622", "cry", ["sad", "tear"]),
        ("\U0001F62D", "sob", ["cry", "bawl", "sad"]),
        ("\U0001F631", "scream", ["shock", "fear"]),
        ("\U0001F613", "downcast_sweat", ["sad"]),
        ("\U0001F629", "weary", ["tired", "frustrated"]),
        ("\U0001F62B", "tired_face", ["exhausted"]),
        ("\U0001F624", "triumph", ["proud", "steam"]),
        ("\U0001F621", "rage", ["angry", "mad"]),
        ("\U0001F620", "angry", ["mad"]),
        ("\U0001F92C", "cursing", ["swear", "angry"]),
        ("\U0001F608", "smiling_imp", ["devil", "evil"]),
        ("\U0001F480", "skull", ["dead", "death"]),
        ("\U0001F4A9", "poop", ["crap"]),
        ("\U0001F921", "clown", ["joker"]),
        ("\U0001F47B", "ghost", ["boo", "halloween"]),
        ("\U0001F47D", "alien", ["ufo"]),
        ("\U0001F916", "robot", ["bot"]),
    ]),
    "People": ("People", "\U0001F44D", [
        ("\U0001F44D", "thumbsup", ["yes", "like", "approve", "+1"]),
        ("\U0001F44E", "thumbsdown", ["no", "dislike", "-1"]),
        ("\U0001F44F", "clap", ["applause", "bravo"]),
        ("\U0001F64C", "raised_hands", ["celebrate", "praise"]),
        ("\U0001F450", "open_hands", ["hug"]),
        ("\U0001F91D", "handshake", ["deal", "agree"]),
        ("\U0001F64F", "pray", ["thanks", "please", "hope"]),
        ("\u270C\uFE0F", "v", ["peace", "victory"]),
        ("\U0001F91E", "crossed_fingers", ["luck", "hope"]),
        ("\U0001F918", "metal", ["rock"]),
        ("\U0001F919", "call_me", ["hang", "loose"]),
        ("\U0001F448", "point_left", []),
        ("\U0001F449", "point_right", []),
        ("\U0001F446", "point_up", []),
        ("\U0001F447", "point_down", []),
        ("\u270B", "raised_hand", ["stop", "high", "five"]),
        ("\U0001F44B", "wave", ["hi", "bye", "hello"]),
        ("\U0001F90F", "pinch", ["small", "tiny"]),
        ("\U0001F4AA", "muscle", ["strong", "flex"]),
        ("\U0001F595", "middle_finger", ["rude"]),
        ("\u270D\uFE0F", "writing_hand", ["write"]),
        ("\U0001F440", "eyes", ["look", "see"]),
        ("\U0001F441\uFE0F", "eye", ["look"]),
        ("\U0001F9E0", "brain", ["smart", "mind"]),
        ("\U0001F476", "baby", ["infant"]),
        ("\U0001F9D2", "child", []),
        ("\U0001F9D1", "adult", ["person"]),
        ("\U0001F468", "man", []),
        ("\U0001F469", "woman", []),
        ("\U0001F9D3", "older_adult", []),
        ("\U0001F9D4", "bearded", ["beard"]),
        ("\U0001F46E", "police", ["cop", "officer"]),
        ("\U0001F575\uFE0F", "detective", ["spy"]),
        ("\U0001F477", "construction_worker", ["builder"]),
        ("\U0001F934", "prince", []),
        ("\U0001F478", "princess", []),
        ("\U0001F9B8", "superhero", ["hero"]),
        ("\U0001F9B9", "supervillain", ["villain"]),
        ("\U0001F9D1\u200D\U0001F4BB", "technologist", ["developer", "coder"]),
        ("\U0001F9D1\u200D\U0001F373", "cook", ["chef"]),
        ("\U0001F9D1\u200D\U0001F680", "astronaut", ["space"]),
        ("\U0001F463", "footprints", ["feet"]),
    ]),
    "Animals": ("Animals & Nature", "\U0001F436", [
        ("\U0001F436", "dog", ["puppy"]),
        ("\U0001F431", "cat", ["kitten"]),
        ("\U0001F42D", "mouse", []),
        ("\U0001F439", "hamster", []),
        ("\U0001F430", "rabbit", ["bunny"]),
        ("\U0001F98A", "fox", []),
        ("\U0001F43B", "bear", []),
        ("\U0001F43C", "panda", []),
        ("\U0001F428", "koala", []),
        ("\U0001F42F", "tiger", []),
        ("\U0001F981", "lion", []),
        ("\U0001F42E", "cow", []),
        ("\U0001F437", "pig", []),
        ("\U0001F438", "frog", []),
        ("\U0001F435", "monkey", []),
        ("\U0001F414", "chicken", []),
        ("\U0001F427", "penguin", []),
        ("\U0001F426", "bird", []),
        ("\U0001F986", "duck", []),
        ("\U0001F989", "owl", []),
        ("\U0001F43A", "wolf", []),
        ("\U0001F434", "horse", []),
        ("\U0001F984", "unicorn", []),
        ("\U0001F41D", "bee", ["honeybee"]),
        ("\U0001F41B", "bug", ["caterpillar"]),
        ("\U0001F98B", "butterfly", []),
        ("\U0001F40C", "snail", []),
        ("\U0001F41E", "lady_beetle", ["ladybug"]),
        ("\U0001F577\uFE0F", "spider", []),
        ("\U0001F422", "turtle", ["tortoise"]),
        ("\U0001F40D", "snake", []),
        ("\U0001F98E", "lizard", []),
        ("\U0001F419", "octopus", []),
        ("\U0001F990", "shrimp", []),
        ("\U0001F980", "crab", []),
        ("\U0001F420", "tropical_fish", ["fish"]),
        ("\U0001F41F", "fish", []),
        ("\U0001F42C", "dolphin", []),
        ("\U0001F433", "whale", []),
        ("\U0001F988", "shark", []),
        ("\U0001F40A", "crocodile", []),
        ("\U0001F418", "elephant", []),
        ("\U0001F992", "giraffe", []),
        ("\U0001F987", "bat", []),
        ("\U0001F338", "cherry_blossom", ["flower", "spring"]),
        ("\U0001F490", "bouquet", ["flowers"]),
        ("\U0001F339", "rose", ["flower"]),
        ("\U0001F33B", "sunflower", ["flower"]),
        ("\U0001F337", "tulip", ["flower"]),
        ("\U0001F332", "evergreen_tree", ["tree", "pine"]),
        ("\U0001F333", "deciduous_tree", ["tree"]),
        ("\U0001F334", "palm_tree", ["tree"]),
        ("\U0001F335", "cactus", []),
        ("\U0001F340", "four_leaf_clover", ["luck"]),
        ("\U0001F341", "maple_leaf", ["autumn", "fall"]),
        ("\U0001F344", "mushroom", []),
        ("\U0001F30D", "earth_africa", ["world", "globe"]),
        ("\U0001F319", "crescent_moon", ["night"]),
        ("\u2B50", "star", []),
        ("\U0001F31F", "star2", ["glowing", "sparkle"]),
        ("\u2728", "sparkles", ["shiny"]),
        ("\u26A1", "zap", ["lightning", "bolt"]),
        ("\U0001F525", "fire", ["lit", "flame"]),
        ("\U0001F308", "rainbow", []),
        ("\u2600\uFE0F", "sunny", ["sun", "weather"]),
        ("\u2601\uFE0F", "cloud", ["weather"]),
        ("\u2744\uFE0F", "snowflake", ["snow", "winter"]),
        ("\u26C4", "snowman", ["snow", "winter"]),
        ("\U0001F4A7", "droplet", ["water", "drop"]),
        ("\U0001F30A", "ocean", ["wave", "water"]),
    ]),
    "Food": ("Food & Drink", "\U0001F354", [
        ("\U0001F34E", "apple", ["fruit"]),
        ("\U0001F34F", "green_apple", ["fruit"]),
        ("\U0001F34A", "tangerine", ["orange", "fruit"]),
        ("\U0001F34B", "lemon", ["fruit"]),
        ("\U0001F34C", "banana", ["fruit"]),
        ("\U0001F349", "watermelon", ["fruit"]),
        ("\U0001F347", "grapes", ["fruit"]),
        ("\U0001F353", "strawberry", ["fruit"]),
        ("\U0001F352", "cherries", ["fruit"]),
        ("\U0001F351", "peach", ["fruit"]),
        ("\U0001F96D", "mango", ["fruit"]),
        ("\U0001F34D", "pineapple", ["fruit"]),
        ("\U0001F95D", "kiwi", ["fruit"]),
        ("\U0001F345", "tomato", []),
        ("\U0001F951", "avocado", []),
        ("\U0001F346", "eggplant", ["aubergine"]),
        ("\U0001F955", "carrot", []),
        ("\U0001F33D", "corn", []),
        ("\U0001F336\uFE0F", "hot_pepper", ["chili", "spicy"]),
        ("\U0001F966", "broccoli", []),
        ("\U0001F35E", "bread", []),
        ("\U0001F950", "croissant", []),
        ("\U0001F9C0", "cheese", []),
        ("\U0001F373", "fried_egg", ["cooking"]),
        ("\U0001F95E", "pancakes", []),
        ("\U0001F953", "bacon", []),
        ("\U0001F354", "hamburger", ["burger"]),
        ("\U0001F35F", "fries", []),
        ("\U0001F355", "pizza", []),
        ("\U0001F32D", "hotdog", []),
        ("\U0001F96A", "sandwich", []),
        ("\U0001F32E", "taco", []),
        ("\U0001F32F", "burrito", []),
        ("\U0001F957", "salad", []),
        ("\U0001F37F", "popcorn", []),
        ("\U0001F35D", "spaghetti", ["pasta"]),
        ("\U0001F35C", "ramen", ["noodles"]),
        ("\U0001F363", "sushi", []),
        ("\U0001F371", "bento", []),
        ("\U0001F35A", "rice", []),
        ("\U0001F366", "icecream", ["dessert"]),
        ("\U0001F369", "doughnut", ["donut"]),
        ("\U0001F36A", "cookie", []),
        ("\U0001F382", "birthday", ["cake"]),
        ("\U0001F370", "cake", ["dessert"]),
        ("\U0001F9C1", "cupcake", []),
        ("\U0001F36B", "chocolate", []),
        ("\U0001F36C", "candy", ["sweet"]),
        ("\U0001F36D", "lollipop", []),
        ("\U0001F36F", "honey", []),
        ("\u2615", "coffee", ["tea", "hot"]),
        ("\U0001F375", "tea", []),
        ("\U0001F964", "cup_with_straw", ["soda", "drink"]),
        ("\U0001F37A", "beer", []),
        ("\U0001F37B", "beers", ["cheers"]),
        ("\U0001F377", "wine", []),
        ("\U0001F378", "cocktail", []),
        ("\U0001F942", "champagne_glass", ["cheers", "toast"]),
        ("\U0001F37E", "champagne_bottle", ["celebrate"]),
    ]),
    "Travel": ("Travel & Places", "\u2708\uFE0F", [
        ("\U0001F697", "car", ["auto"]),
        ("\U0001F695", "taxi", []),
        ("\U0001F68C", "bus", []),
        ("\U0001F691", "ambulance", []),
        ("\U0001F692", "fire_engine", []),
        ("\U0001F693", "police_car", []),
        ("\U0001F3CE\uFE0F", "racing_car", ["race"]),
        ("\U0001F69A", "truck", []),
        ("\U0001F6B2", "bike", ["bicycle"]),
        ("\U0001F6F5", "motor_scooter", ["scooter"]),
        ("\U0001F3CD\uFE0F", "motorcycle", []),
        ("\u2708\uFE0F", "airplane", ["plane", "flight", "travel"]),
        ("\U0001F680", "rocket", ["launch", "ship"]),
        ("\U0001F6F8", "flying_saucer", ["ufo"]),
        ("\U0001F681", "helicopter", []),
        ("\u26F5", "sailboat", ["boat"]),
        ("\U0001F6A4", "speedboat", ["boat"]),
        ("\U0001F6A2", "ship", []),
        ("\u2693", "anchor", []),
        ("\U0001F686", "train", []),
        ("\U0001F687", "metro", ["subway"]),
        ("\U0001F5FA\uFE0F", "map", []),
        ("\U0001F9ED", "compass", []),
        ("\U0001F3D4\uFE0F", "mountain_snow", ["mountain"]),
        ("\U0001F30B", "volcano", []),
        ("\U0001F3D5\uFE0F", "camping", ["tent"]),
        ("\U0001F3D6\uFE0F", "beach", ["vacation"]),
        ("\U0001F3DD\uFE0F", "desert_island", ["island"]),
        ("\U0001F3D9\uFE0F", "cityscape", ["city"]),
        ("\U0001F303", "night_city", ["night"]),
        ("\U0001F305", "sunrise", []),
        ("\U0001F309", "bridge_at_night", ["bridge"]),
        ("\U0001F3E0", "house", ["home"]),
        ("\U0001F3E1", "house_with_garden", ["home"]),
        ("\U0001F3E2", "office", ["building"]),
        ("\U0001F3E5", "hospital", []),
        ("\U0001F3E6", "bank", []),
        ("\U0001F3E8", "hotel", []),
        ("\U0001F3EB", "school", []),
        ("\U0001F3F0", "castle", []),
        ("\u26EA", "church", []),
        ("\U0001F5FC", "tokyo_tower", ["tower"]),
        ("\U0001F5FD", "statue_of_liberty", []),
        ("\U0001F3A1", "ferris_wheel", []),
        ("\U0001F3A2", "roller_coaster", []),
        ("\u26F2", "fountain", []),
        ("\U0001F3AA", "circus_tent", ["circus"]),
        ("\U0001F3AD", "performing_arts", ["theater", "masks"]),
    ]),
    "Activities": ("Activities", "\u26BD", [
        ("\u26BD", "soccer", ["football", "ball"]),
        ("\U0001F3C0", "basketball", ["ball"]),
        ("\U0001F3C8", "football", ["american", "ball"]),
        ("\u26BE", "baseball", ["ball"]),
        ("\U0001F3BE", "tennis", ["ball"]),
        ("\U0001F3D0", "volleyball", ["ball"]),
        ("\U0001F3C9", "rugby", ["ball"]),
        ("\U0001F3B1", "8ball", ["pool", "billiards"]),
        ("\U0001F3D3", "ping_pong", ["table", "tennis"]),
        ("\U0001F3F8", "badminton", []),
        ("\U0001F3D2", "hockey", []),
        ("\u26F3", "golf", ["flag"]),
        ("\U0001F3F9", "bow_and_arrow", ["archery"]),
        ("\U0001F3A3", "fishing", []),
        ("\U0001F94A", "boxing", ["glove"]),
        ("\U0001F94B", "martial_arts", []),
        ("\u26F8\uFE0F", "ice_skate", ["skating"]),
        ("\U0001F3BF", "ski", []),
        ("\U0001F3C2", "snowboard", []),
        ("\U0001F3CB\uFE0F", "weight_lifter", ["gym", "lifting"]),
        ("\U0001F938", "cartwheel", ["gymnastics"]),
        ("\U0001F3CC\uFE0F", "golfer", ["golf"]),
        ("\U0001F9D7", "climbing", []),
        ("\U0001F6B4", "biking", ["cyclist"]),
        ("\U0001F3CA", "swimmer", ["swimming"]),
        ("\U0001F3AF", "dart", ["target", "bullseye"]),
        ("\U0001F3B3", "bowling", []),
        ("\U0001F3AE", "video_game", ["gaming", "controller"]),
        ("\U0001F579\uFE0F", "joystick", ["gaming"]),
        ("\U0001F3B2", "game_die", ["dice"]),
        ("\U0001F9E9", "puzzle", ["jigsaw"]),
        ("\U0001F3B0", "slot_machine", ["gamble"]),
        ("\U0001F3A8", "art", ["palette", "paint"]),
        ("\U0001F3AC", "clapper", ["movie", "film"]),
        ("\U0001F3A4", "microphone", ["sing", "karaoke"]),
        ("\U0001F3A7", "headphones", ["music", "listen"]),
        ("\U0001F3B9", "musical_keyboard", ["piano"]),
        ("\U0001F941", "drum", []),
        ("\U0001F3B8", "guitar", []),
        ("\U0001F3BA", "trumpet", []),
        ("\U0001F3BB", "violin", []),
        ("\U0001F3B7", "saxophone", ["sax"]),
        ("\U0001F3AB", "ticket", []),
        ("\U0001F3C6", "trophy", ["win", "award"]),
        ("\U0001F3C5", "medal", []),
        ("\U0001F947", "first_place", ["gold", "medal"]),
        ("\U0001F948", "second_place", ["silver", "medal"]),
        ("\U0001F949", "third_place", ["bronze", "medal"]),
        ("\U0001F389", "tada", ["party", "celebrate", "hooray"]),
        ("\U0001F38A", "confetti_ball", ["party"]),
        ("\U0001F388", "balloon", ["party"]),
        ("\U0001F381", "gift", ["present"]),
        ("\U0001F380", "ribbon", ["bow"]),
    ]),
    "Objects": ("Objects", "\U0001F4A1", [
        ("\U0001F4F1", "iphone", ["phone", "mobile"]),
        ("\U0001F4BB", "laptop", ["computer"]),
        ("\U0001F5A5\uFE0F", "desktop_computer", ["computer"]),
        ("\u2328\uFE0F", "keyboard", []),
        ("\U0001F5A8\uFE0F", "printer", []),
        ("\U0001F4F7", "camera", ["photo"]),
        ("\U0001F4F9", "video_camera", []),
        ("\U0001F3A5", "movie_camera", ["film"]),
        ("\U0001F4FA", "tv", ["television"]),
        ("\U0001F399\uFE0F", "studio_microphone", ["podcast", "mic"]),
        ("\u23F0", "alarm_clock", ["time"]),
        ("\u23F1\uFE0F", "stopwatch", ["timer"]),
        ("\u231A", "watch", ["time"]),
        ("\U0001F50B", "battery", ["power"]),
        ("\U0001F50C", "electric_plug", ["power"]),
        ("\U0001F4A1", "bulb", ["idea", "light"]),
        ("\U0001F526", "flashlight", ["torch"]),
        ("\U0001F56F\uFE0F", "candle", []),
        ("\U0001F9EF", "fire_extinguisher", []),
        ("\U0001F4B8", "money_with_wings", ["spend", "cash"]),
        ("\U0001F4B5", "dollar", ["money", "cash"]),
        ("\U0001F4B0", "moneybag", ["money"]),
        ("\U0001F4B3", "credit_card", ["payment"]),
        ("\U0001F48E", "gem", ["diamond", "jewel"]),
        ("\u2696\uFE0F", "balance_scale", ["justice", "law"]),
        ("\U0001F527", "wrench", ["tool", "fix"]),
        ("\U0001F528", "hammer", ["tool"]),
        ("\U0001F6E0\uFE0F", "hammer_and_wrench", ["tools"]),
        ("\U0001F9F0", "toolbox", ["tools"]),
        ("\u2699\uFE0F", "gear", ["settings", "cog"]),
        ("\U0001F9F2", "magnet", []),
        ("\U0001F52C", "microscope", ["science"]),
        ("\U0001F52D", "telescope", ["space"]),
        ("\U0001F48A", "pill", ["medicine"]),
        ("\U0001F489", "syringe", ["vaccine", "shot"]),
        ("\U0001FA79", "adhesive_bandage", ["bandaid"]),
        ("\U0001F6AA", "door", []),
        ("\U0001F6CF\uFE0F", "bed", []),
        ("\U0001F6CB\uFE0F", "couch", ["sofa"]),
        ("\U0001F6BF", "shower", []),
        ("\U0001F9FC", "soap", []),
        ("\U0001F511", "key", []),
        ("\U0001F512", "lock", ["secure"]),
        ("\U0001F513", "unlock", []),
        ("\U0001F514", "bell", ["notification"]),
        ("\U0001F515", "no_bell", ["mute"]),
        ("\U0001F4E2", "loudspeaker", ["announce"]),
        ("\U0001F4E3", "megaphone", ["shout"]),
        ("\U0001F4CC", "pushpin", ["pin"]),
        ("\U0001F4CD", "round_pushpin", ["location", "pin"]),
        ("\U0001F4CE", "paperclip", ["attach"]),
        ("\u2702\uFE0F", "scissors", ["cut"]),
        ("\U0001F4CF", "straight_ruler", ["measure"]),
        ("\U0001F58A\uFE0F", "pen", []),
        ("\u270F\uFE0F", "pencil", ["write"]),
        ("\U0001F4DD", "memo", ["note", "write"]),
        ("\U0001F4CB", "clipboard", []),
        ("\U0001F4C1", "folder", []),
        ("\U0001F4C2", "open_folder", ["folder"]),
        ("\U0001F4C5", "calendar", ["date"]),
        ("\U0001F4CA", "bar_chart", ["graph", "stats"]),
        ("\U0001F4C8", "chart_increasing", ["graph", "up", "growth"]),
        ("\U0001F4C9", "chart_decreasing", ["graph", "down"]),
        ("\U0001F4D6", "open_book", ["read"]),
        ("\U0001F4DA", "books", ["library"]),
        ("\U0001F516", "bookmark", []),
        ("\U0001F4F0", "newspaper", ["news"]),
        ("\u2709\uFE0F", "envelope", ["mail", "email"]),
        ("\U0001F4E7", "email", ["mail"]),
        ("\U0001F4E6", "package", ["box", "parcel"]),
        ("\U0001F3F7\uFE0F", "label", ["tag"]),
        ("\U0001F9FE", "receipt", ["invoice"]),
    ]),
    "Symbols": ("Symbols", "\u2764\uFE0F", [
        ("\u2764\uFE0F", "heart", ["love", "red"]),
        ("\U0001F9E1", "orange_heart", []),
        ("\U0001F49B", "yellow_heart", []),
        ("\U0001F49A", "green_heart", []),
        ("\U0001F499", "blue_heart", []),
        ("\U0001F49C", "purple_heart", []),
        ("\U0001F5A4", "black_heart", []),
        ("\U0001F90D", "white_heart", []),
        ("\U0001F90E", "brown_heart", []),
        ("\U0001F494", "broken_heart", ["heartbreak"]),
        ("\U0001F495", "two_hearts", ["love"]),
        ("\U0001F496", "sparkling_heart", ["love"]),
        ("\U0001F498", "cupid", ["love", "arrow"]),
        ("\U0001F49D", "gift_heart", ["love"]),
        ("\U0001F4AF", "100", ["hundred", "perfect", "score"]),
        ("\u2705", "white_check_mark", ["check", "yes", "done"]),
        ("\u2714\uFE0F", "heavy_check_mark", ["check", "done"]),
        ("\u2611\uFE0F", "ballot_box_with_check", ["check"]),
        ("\u274C", "x", ["no", "wrong", "cross"]),
        ("\u2B55", "o", ["circle", "correct"]),
        ("\u2757", "exclamation", ["warning", "important"]),
        ("\u2753", "question", ["help"]),
        ("\u203C\uFE0F", "bangbang", ["double", "exclamation"]),
        ("\u26A0\uFE0F", "warning", ["caution", "alert"]),
        ("\U0001F6AB", "no_entry_sign", ["ban", "forbidden"]),
        ("\u267B\uFE0F", "recycle", []),
        ("\u00A9\uFE0F", "copyright", []),
        ("\u00AE\uFE0F", "registered", []),
        ("\u2122\uFE0F", "tm", ["trademark"]),
        ("\U0001F51F", "keycap_ten", ["10"]),
        ("\U0001F522", "1234", ["numbers"]),
        ("\U0001F524", "abc", ["letters"]),
        ("\U0001F197", "ok", ["okay"]),
        ("\U0001F195", "new", []),
        ("\U0001F193", "free", []),
        ("\u267F", "wheelchair", ["accessible"]),
        ("\U0001F534", "red_circle", []),
        ("\U0001F7E0", "orange_circle", []),
        ("\U0001F7E1", "yellow_circle", []),
        ("\U0001F7E2", "green_circle", []),
        ("\U0001F535", "blue_circle", []),
        ("\U0001F7E3", "purple_circle", []),
        ("\u26AB", "black_circle", []),
        ("\u26AA", "white_circle", []),
        ("\U0001F536", "large_orange_diamond", []),
        ("\U0001F537", "large_blue_diamond", []),
        ("\u25B6\uFE0F", "arrow_forward", ["play"]),
        ("\u23F8\uFE0F", "pause_button", ["pause"]),
        ("\u23F9\uFE0F", "stop_button", ["stop"]),
        ("\u23ED\uFE0F", "next_track", ["skip"]),
        ("\U0001F500", "twisted_rightwards_arrows", ["shuffle"]),
        ("\U0001F501", "repeat", ["loop"]),
        ("\u27A1\uFE0F", "arrow_right", []),
        ("\u2B05\uFE0F", "arrow_left", []),
        ("\u2B06\uFE0F", "arrow_up", []),
        ("\u2B07\uFE0F", "arrow_down", []),
        ("\U0001F504", "arrows_counterclockwise", ["refresh", "sync"]),
        ("\u2795", "heavy_plus_sign", ["plus", "add"]),
        ("\u2796", "heavy_minus_sign", ["minus"]),
        ("\u2716\uFE0F", "heavy_multiplication_x", ["multiply", "times"]),
        ("\u267E\uFE0F", "infinity", []),
        ("\U0001F4B2", "heavy_dollar_sign", ["money"]),
    ]),
    "Flags": ("Flags", "\U0001F3C1", [
        ("\U0001F3C1", "checkered_flag", ["race", "finish"]),
        ("\U0001F6A9", "triangular_flag", ["red", "flag"]),
        ("\U0001F38C", "crossed_flags", []),
        ("\U0001F3F4", "black_flag", []),
        ("\U0001F3F3\uFE0F", "white_flag", ["surrender"]),
        ("\U0001F3F3\uFE0F\u200D\U0001F308", "rainbow_flag", ["pride"]),
        ("\U0001F3F4\u200D\u2620\uFE0F", "pirate_flag", ["pirate"]),
    ]),
}

def rs_str(s):
    # Emit a Rust string literal with unicode escapes for the emoji chars.
    out = []
    for ch in s:
        cp = ord(ch)
        if ch in ('"', '\\'):
            out.append('\\' + ch)
        elif 0x20 <= cp < 0x7f:
            out.append(ch)
        else:
            out.append('\\u{%x}' % cp)
    return '"' + ''.join(out) + '"'

def main():
    # uniqueness check
    seen = {}
    total = 0
    for gkey, (label, tab, rows) in DATA.items():
        for ch, code, kws in rows:
            total += 1
            if code in seen:
                print("DUPLICATE shortcode: %s" % code, file=sys.stderr)
                sys.exit(1)
            seen[code] = True

    L = []
    L.append("//! Shared emoji dataset + fuzzy search — the single source of emoji")
    L.append("//! data in the toolkit. Both the [`EmojiPicker`](crate::widget::EmojiPicker)")
    L.append("//! widget and the opt-in `:shortcode:` completion on")
    L.append("//! [`TextInput`](crate::widget::TextInput) read from here, so no app")
    L.append("//! (nor the on-screen keyboard) re-implements an emoji table.")
    L.append("//!")
    L.append("//! GENERATED by scripts/gen_emoji.py — edit the table there, not this file.")
    L.append("")
    L.append("use crate::search::fuzzy_score;")
    L.append("")
    L.append("/// A top-level emoji category. `Recent` is not a data group — the picker")
    L.append("/// synthesises it from usage — so it is not part of [`Group::ALL`].")
    L.append("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]")
    L.append("pub enum Group {")
    for gkey in DATA:
        L.append("    %s," % gkey)
    L.append("}")
    L.append("")
    L.append("impl Group {")
    L.append("    /// Every data-backed category, in display order.")
    L.append("    pub const ALL: &'static [Group] = &[")
    L.append("        " + ", ".join("Group::%s" % g for g in DATA) + ",")
    L.append("    ];")
    L.append("")
    L.append("    /// Human-readable section title (already i18n-neutral English).")
    L.append("    pub fn label(self) -> &'static str {")
    L.append("        match self {")
    for gkey, (label, tab, rows) in DATA.items():
        L.append("            Group::%s => %s," % (gkey, rs_str(label)))
    L.append("        }")
    L.append("    }")
    L.append("")
    L.append("    /// A representative glyph used on the category tab strip.")
    L.append("    pub fn tab_glyph(self) -> &'static str {")
    L.append("        match self {")
    for gkey, (label, tab, rows) in DATA.items():
        L.append("            Group::%s => %s," % (gkey, rs_str(tab)))
    L.append("        }")
    L.append("    }")
    L.append("}")
    L.append("")
    L.append("/// One emoji entry: the glyph, its canonical `:shortcode:` name, search")
    L.append("/// keywords, and its category.")
    L.append("#[derive(Clone, Copy, Debug)]")
    L.append("pub struct Emoji {")
    L.append("    pub ch: &'static str,")
    L.append("    pub name: &'static str,")
    L.append("    pub keywords: &'static [&'static str],")
    L.append("    pub group: Group,")
    L.append("}")
    L.append("")
    L.append("/// The full curated dataset.")
    L.append("pub static EMOJI: &[Emoji] = &[")
    for gkey, (label, tab, rows) in DATA.items():
        for ch, code, kws in rows:
            kw = "&[" + ", ".join(rs_str(k) for k in kws) + "]"
            L.append("    Emoji { ch: %s, name: %s, keywords: %s, group: Group::%s }," % (
                rs_str(ch), rs_str(code), kw, gkey))
    L.append("];")
    L.append("")
    L.append("/// Every emoji in `group`, in table order.")
    L.append("pub fn in_group(group: Group) -> impl Iterator<Item = &'static Emoji> {")
    L.append("    EMOJI.iter().filter(move |e| e.group == group)")
    L.append("}")
    L.append("")
    L.append("/// The emoji whose canonical shortcode is exactly `code` (no `:`), if any.")
    L.append("/// Used by the `:shortcode:` auto-replace when the user types the closing")
    L.append("/// colon.")
    L.append("pub fn exact(code: &str) -> Option<&'static Emoji> {")
    L.append("    EMOJI.iter().find(|e| e.name == code)")
    L.append("}")
    L.append("")
    L.append("/// Best fuzzy score for `query` against an emoji's shortcode (weighted")
    L.append("/// higher) or any of its keywords. `None` when nothing matches.")
    L.append("pub fn score(query: &str, e: &Emoji) -> Option<i32> {")
    L.append("    // A shortcode hit outranks a keyword hit at equal raw score.")
    L.append("    let mut best = fuzzy_score(query, e.name).map(|s| s + 6);")
    L.append("    for kw in e.keywords {")
    L.append("        if let Some(s) = fuzzy_score(query, kw) {")
    L.append("            best = Some(best.map_or(s, |b| b.max(s)));")
    L.append("        }")
    L.append("    }")
    L.append("    best")
    L.append("}")
    L.append("")
    L.append("/// Fuzzy-search the whole dataset, best matches first, capped at `limit`.")
    L.append("/// An empty query returns nothing (callers show categories instead).")
    L.append("pub fn search(query: &str, limit: usize) -> Vec<&'static Emoji> {")
    L.append("    let q = query.trim();")
    L.append("    if q.is_empty() {")
    L.append("        return Vec::new();")
    L.append("    }")
    L.append("    let mut scored: Vec<(i32, usize, &'static Emoji)> = EMOJI")
    L.append("        .iter()")
    L.append("        .enumerate()")
    L.append("        .filter_map(|(i, e)| score(q, e).map(|s| (s, i, e)))")
    L.append("        .collect();")
    L.append("    // Highest score first; stable by table order on ties.")
    L.append("    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));")
    L.append("    scored.into_iter().take(limit).map(|(_, _, e)| e).collect()")
    L.append("}")
    L.append("")
    L.append("#[cfg(test)]")
    L.append("mod tests {")
    L.append("    use super::*;")
    L.append("    use std::collections::HashSet;")
    L.append("")
    L.append("    #[test]")
    L.append("    fn shortcodes_are_unique() {")
    L.append("        let mut seen = HashSet::new();")
    L.append("        for e in EMOJI {")
    L.append("            assert!(seen.insert(e.name), \"duplicate shortcode: {}\", e.name);")
    L.append("        }")
    L.append("    }")
    L.append("")
    L.append("    #[test]")
    L.append("    fn every_group_has_entries() {")
    L.append("        for g in Group::ALL {")
    L.append("            assert!(in_group(*g).next().is_some(), \"empty group: {:?}\", g);")
    L.append("        }")
    L.append("    }")
    L.append("")
    L.append("    #[test]")
    L.append("    fn search_filters_by_name_and_keyword() {")
    L.append("        // Name match.")
    L.append("        let r = search(\"smi\", 20);")
    L.append("        assert!(r.iter().any(|e| e.name == \"smile\"), \"':smi' should suggest :smile:\");")
    L.append("        // Keyword match: 'lol' is a keyword of joy/laughing, not a shortcode.")
    L.append("        let r = search(\"lol\", 20);")
    L.append("        assert!(r.iter().any(|e| e.name == \"joy\" || e.name == \"laughing\"),")
    L.append("            \"'lol' keyword should surface a laughing face\");")
    L.append("        // Empty query yields nothing (categories are shown instead).")
    L.append("        assert!(search(\"\", 20).is_empty());")
    L.append("    }")
    L.append("")
    L.append("    #[test]")
    L.append("    fn exact_shortcode_lookup() {")
    L.append("        assert_eq!(exact(\"fire\").map(|e| e.ch), Some(\"\\u{1f525}\"));")
    L.append("        assert!(exact(\"definitely_not_an_emoji\").is_none());")
    L.append("    }")
    L.append("}")
    L.append("")
    sys.stdout.write("\n".join(L))
    print("// total emoji: %d" % total, file=sys.stderr)

main()
