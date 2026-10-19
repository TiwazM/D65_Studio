"""Build one MEGA65 HD floppy track as raw flux for DMA option $0D.

The layout copies the core's own hardware format (sdcardio.vhdl,
FDCAutoFormatTrack, "with gaps"): 14 x $00, a Track Info Block, 2 x $4E,
then per sector 3 x A1 sync, $FE, track, side, sector, $02, CRC, 23 x $4E,
12 x $00, 3 x A1 sync, $FB, 512 data bytes, CRC, 24 x $4E. Only the sector
contents differ: they come from the caller instead of the format pattern.

DMA option $0D emits one flux transition per source byte and waits 2 x value
cycles before the next one (gs4510.vhdl, DMAgicCopyFloppyWrite), plus a fixed
per-byte overhead that `overhead` subtracts.
"""


def crc16(data, crc=0xFFFF):
    for b in data:
        crc ^= b << 8
        for _ in range(8):
            crc = ((crc << 1) ^ 0x1021) if crc & 0x8000 else crc << 1
            crc &= 0xFFFF
    return crc


class Cells:
    """MFM cell stream: two cells (clock, data) per data bit."""

    def __init__(self):
        self.cells = []
        self.prev = 0

    def byte(self, b):
        for i in range(7, -1, -1):
            d = b >> i & 1
            self.cells += [1 if (self.prev == 0 and d == 0) else 0, d]
            self.prev = d

    def bytes(self, data):
        for b in data:
            self.byte(b)

    def sync_a1(self):
        # A1 with the clock bit between data bits 4 and 5 missing: $4489.
        for i in range(15, -1, -1):
            self.cells.append(0x4489 >> i & 1)
        self.prev = 1


def track_cells(track, sectors, rate, side=0, encoding=0x40, tib_sectors=None, gap_marks=None):
    """sectors: list of 512-byte blocks, numbered from 1. gap_marks, if
    given, receives the cell offset where each sector's trailing gap starts."""
    c = Cells()
    c.bytes([0x00] * 14)
    tib = [0x65, track, rate, encoding, tib_sectors if tib_sectors is not None else len(sectors)]
    for _ in range(3):
        c.sync_a1()
    crc = crc16(bytes([0xA1] * 3 + tib))
    c.bytes(tib + [crc >> 8, crc & 0xFF])
    c.bytes([0x00] * 4 + [0x4E] * 2)
    for n, data in enumerate(sectors, start=1):
        assert len(data) == 512
        header = [0xFE, track, side, n, 0x02]
        for _ in range(3):
            c.sync_a1()
        crc = crc16(bytes([0xA1] * 3 + header))
        c.bytes(header + [crc >> 8, crc & 0xFF])
        c.bytes([0x4E] * 23 + [0x00] * 12)
        for _ in range(3):
            c.sync_a1()
        body = bytes([0xFB]) + bytes(data)
        crc = crc16(bytes([0xA1] * 3) + body)
        c.bytes(list(body) + [crc >> 8, crc & 0xFF])
        if gap_marks is not None:
            gap_marks.append(len(c.cells))
        c.bytes([0x4E] * 24)
    return c.cells


def split_point(cells, gap_marks, limit=65535):
    """A flux index at most `limit` in the middle of a sector's trailing $4E
    gap, where the short pause between two chained DMA jobs does no harm."""
    best = None
    for mark in gap_marks:
        middle = mark + 12 * 16
        index = sum(cells[:middle])
        if index <= limit:
            best = index
    return best


def flux(cells, rate, overhead=0):
    """DMA source bytes: one per flux transition, value = interval / 2."""
    out = []
    gap = 0
    for cell in cells:
        gap += 1
        if cell:
            cycles = gap * rate - overhead
            v = max(1, min(255, round(cycles / 2)))
            out.append(v)
            gap = 0
    return bytes(out)


def revolution_cycles(rpm=300, clock=40_500_000):
    return clock * 60 // rpm


if __name__ == "__main__":
    data = [bytes((s * 7 + i) & 0xFF for i in range(512)) for s in range(1, 22)]
    cells = track_cells(40, data, 40)
    f = flux(cells, 40)
    used = len(cells) * 40
    print(f"{len(cells)} cells, {len(f)} flux bytes, {used} of {revolution_cycles()} cycles"
          f" ({100 * used / revolution_cycles():.1f}% of a revolution)")
    # Every MFM interval must be 2, 3 or 4 cells.
    gaps = sorted(set(f))
    print("distinct DMA values:", gaps)
