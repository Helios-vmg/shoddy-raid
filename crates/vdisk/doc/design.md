vdisk

# Description

This is a simple virtual disk library intended for use in file systems without native sparse file support. It’s meant to be straightforward to implement, nothing fancy.

# File format

struct File\{  
    Header header;  
    u64 block\_table\[header.block\_count\];  
    Block blocks\[header.allocated\_block\_count\];  
\};  
  
struct Header\{ // Padded to 4K  
    u8 magic\_number\[4\]; // "GNAF"  
    u32 version;  
    u64 block\_count;  
    u64 allocated\_block\_count;  
\};  
  
typedef u8 Block\[1 \<\< 20\];

Integers are encoded in little endian.
