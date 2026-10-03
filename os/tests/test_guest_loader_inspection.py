import importlib.util,io,tarfile,unittest
from pathlib import Path
spec=importlib.util.spec_from_file_location('inspect_guest',Path(__file__).parents[1]/'dev/inspect-retained-guest-loaders.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
class Inspection(unittest.TestCase):
 def test_loader_archive_inspection(self):
  with self.subTest(phase="symlink_is_not_followed_or_extracted"):
   stream=io.BytesIO()
   with tarfile.open(fileobj=stream,mode='w') as tar:
    item=tarfile.TarInfo('wine');item.type=tarfile.SYMTYPE;item.linkname='../../lib/wine/i386-unix/wine';tar.addfile(item)
   record,data=m.member(stream.getvalue());self.assertEqual(record['target'],'../../lib/wine/i386-unix/wine');self.assertIsNone(data)
  with self.subTest(phase="architecture_checked_from_bytes"):
   for width,machine,bits in [(1,3,32),(2,62,64)]:
    data=bytearray(64);data[:6]=b'\x7fELF'+bytes([width,1]);data[18:20]=machine.to_bytes(2,'little');self.assertEqual(m.elf(data)['bits'],bits)
   data[18:20]=(183).to_bytes(2,'little')
   with self.assertRaises(ValueError):m.elf(data)
  with self.subTest(phase="multifile_tar_rejected"):
   stream=io.BytesIO()
   with tarfile.open(fileobj=stream,mode='w') as tar:
    for name in ['a','b']:tar.addfile(tarfile.TarInfo(name))
   with self.assertRaises(ValueError):m.member(stream.getvalue())
if __name__=='__main__':unittest.main()
