"""Read PE Machine from exact installed bytes; no execution or package changes."""
from pathlib import Path
import struct
import sys
with Path(sys.argv[1]).open('rb') as f:
    header=f.read(64)
    assert len(header)==64 and header[:2]==b'MZ'
    offset=struct.unpack_from('<I',header,60)[0]
    assert 64<=offset<16*1024*1024
    f.seek(offset);pe=f.read(6)
    assert pe[:4]==b'PE\0\0' and struct.unpack_from('<H',pe,4)[0]==0x8664,'Actual executable is not x64 PE'
print('x86_64')
