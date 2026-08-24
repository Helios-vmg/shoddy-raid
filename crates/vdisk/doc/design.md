vdisk

# Description

This is a simple virtual disk library intended for use in file systems without native sparse file support. It’s meant to be straightforward to implement, nothing fancy.

# File format

```
struct File{
    Header header;
    u64 block_table[header.block_count];
    Block blocks[header.allocated_block_count];
};

struct Header{ // Padded to 4K
    u8 magic_number[4]; // "GNAF"
    u32 version;
    u8 serial[32];
    u64 block_count;
    u64 allocated_block_count;
};

typedef u8 Block[1 << 20];
```

Integers are encoded in little endian.
