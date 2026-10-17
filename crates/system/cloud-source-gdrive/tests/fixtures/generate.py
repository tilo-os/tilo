"""Regenerate deterministic Office packages served by FakeDrive.

Uses only Python's standard library; fixed timestamps and stored entries keep
fixture bytes independent of compression-library versions.
"""
from io import BytesIO
from pathlib import Path
from zipfile import ZIP_STORED, ZipFile, ZipInfo

ROOT = Path(__file__).parent
REL = "http://schemas.openxmlformats.org/package/2006/relationships"
OFFICE = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"


def relationships(entries):
    return f'<Relationships xmlns="{REL}">' + ''.join(
        f'<Relationship Id="{ident}" Type="{OFFICE}/{kind}" Target="{target}"/>'
        for ident, kind, target in entries
    ) + '</Relationships>'


def package(extension, main, content_type, parts):
    types = ('<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
             '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
             '<Default Extension="xml" ContentType="application/xml"/>'
             f'<Override PartName="/{main}" ContentType="{content_type}"/>')
    for name, (_, kind) in parts.items():
        if kind:
            types += f'<Override PartName="/{name}" ContentType="{kind}"/>'
    files = {
        '[Content_Types].xml': types + '</Types>',
        '_rels/.rels': relationships([('rId1', 'officeDocument', main)]),
    }
    files.update({name: xml for name, (xml, _) in parts.items()})
    buffer = BytesIO()
    with ZipFile(buffer, 'w', compression=ZIP_STORED) as archive:
        for name, xml in sorted(files.items()):
            info = ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = ZIP_STORED
            info.external_attr = 0o644 << 16
            archive.writestr(info, '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>' + xml)
    literal = ''.join(
        chr(byte) if 32 <= byte < 127 and byte not in (34, 92)
        else '\\"' if byte == 34
        else '\\\\' if byte == 92
        else f'\\x{byte:02x}'
        for byte in buffer.getvalue()
    )
    (ROOT / f'{extension}.rs').write_text(
        '// Genuine Office package represented as text for the text-only repository.\n'
        f'pub const BYTES: &[u8] = b"{literal}";\n',
        encoding='ascii',
    )


package('docx', 'word/document.xml',
        'application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml', {
    'word/document.xml': ('<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Cloud document export fixture</w:t></w:r></w:p><w:sectPr/></w:body></w:document>', None),
})

package('xlsx', 'xl/workbook.xml',
        'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml', {
    'xl/workbook.xml': (f'<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="{OFFICE}"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>', None),
    'xl/_rels/workbook.xml.rels': (relationships([('rId1', 'worksheet', 'worksheets/sheet1.xml')]), None),
    'xl/worksheets/sheet1.xml': ('<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Cloud spreadsheet export fixture</t></is></c></row></sheetData></worksheet>', 'application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml'),
})

# The standalone slide contains a text box and no master-dependent content.
package('pptx', 'ppt/presentation.xml',
        'application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml', {
    'ppt/presentation.xml': (f'<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="{OFFICE}"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>', None),
    'ppt/_rels/presentation.xml.rels': (relationships([('rId1', 'slide', 'slides/slide1.xml')]), None),
    'ppt/slides/slide1.xml': ('<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/><p:sp><p:nvSpPr><p:cNvPr id="2" name="Export text"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="9144000" cy="6858000"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>Cloud presentation export fixture</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>', 'application/vnd.openxmlformats-officedocument.presentationml.slide+xml'),
})
