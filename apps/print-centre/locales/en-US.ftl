## History

print-centre-print-defaults-two-sided = { $new ->
    [true] Printing on both sides from now on
   *[false] Printing on one side from now on
}
print-centre-print-defaults-scan-resolution = Scanning at { $new } dpi from now on
print-centre-print-defaults-scan-color = { $new ->
    [grayscale] Scanning in grayscale from now on
    [line-art] Scanning in line art from now on
   *[color] Scanning in color from now on
}
print-centre-print-defaults-scan-source = { $new ->
    [feeder] Scanning from the feeder from now on
    [feeder-duplex] Scanning both sides from the feeder from now on
   *[flatbed] Scanning from the flatbed from now on
}
print-centre-print-defaults-ocr-language = { $new ->
    [deu] Recognizing German text from now on
    [fra] Recognizing French text from now on
    [spa] Recognizing Spanish text from now on
   *[eng] Recognizing English text from now on
}
