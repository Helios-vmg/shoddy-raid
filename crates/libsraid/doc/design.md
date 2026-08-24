# Problem statement

You have four 4 TB drives. They “work”, but they all have some surface errors, such that you can write to them, but for some regions of the disk the data you read back is not the same as the one you wrote, but most of each disk is fine. You want to keep using these disks for low priority data that you accept may get lost. ZFS is no help, because once a disk crosses a threshold of errors, it drops it; it’s designed with the assumption that errors will be random and rare, not consistent.

# Proposed solution

Bundle the disks (let’s call them A, B, C, and D) into a single pool. One of the disks will be used for parity, such that for every byte at the same physical address, D = XOR(A, B, C). This makes it so that, per address, if a single disk contains corruption, its contents can be reconstructed from the other three disks. The problem is, when there is corruption, to detect which disk contains the error. Instead of doing this per byte, we’ll do it per 4K block, and use a 256-bit hash function (which is yet to be decided; BLAKE3 is a candidate) to validate it. 256 bits = 32 bytes, so we require 4128 bytes to store 4096 bytes (~0.7% more). After taking away the parity, this leaves us with 10.83 TiB usable, out of the theoretical maximum 14.55 TiB (assuming 4E+12 byte disks).

We still need to store filesystem metadata. An SQLite DB can be used for this, stored separately in reliable storage.

# Implementation details

## Physical layout

Disks will be divided into 516 KiB (exactly) **blocks**. That’s 512 KiB + 4 KiB. Remainder space is unusable. The first 512K of each block is the data **section**, while the last 4K is the hash section. The data section is itself divided into 128 4 KiB **subblocks**. Each subblock is matched with a 32-byte hash stored in the hash section. Note that 32\*128=4096.

This layout is mirrored across all disks in a pool. If disks of different sizes are used, only the lower address space that’s valid across all disks can be used. The space resulting from combining all the blocks starting at the same address across all disks is called a **superblock**. Note that the physical size of a superblock is 516 KiB \* (total disks), while the logical size is 512 KiB \* (total disks – 1) (the logical size is the space actually available for user data). For example, with 4 disks, the physical size of a superblock is 2.015625 MiB, while the logical size is 1.5 MiB.

There is one more relevant division that’s not a straight subdivision of any of the previous ones. Within each superblock there are 129 **vertical supersubblocks** (VSSB), each one of which is composed of all the subblocks that start at the same physical address across all the disks.

## Physical allocation

Since hashes are stored contiguously in a single 4K subblock, in order to minimize rewrites at the physical level, space will be allocated at the superblock level. That means for a 4 disk pool the smallest storable file will be 1.5 MiB long. It also means that a file 1.500001 MiB long will require 3 MiB of logical space. Additional abstractions may be implemented on top to support storing small files efficiently.

## Operations

The IO interface will be narrowly defined, and will support the following operations:

- Write a new file

- Read a file

- Delete a file

- Perform an integrity check on the whole pool, or some selection of files

- Other metadata queries

## Write procedure

This is the high level description of the procedure. Parallelization optimizations will not be described.

1 Check for space on the pool. A file of size S requires n = (S + B – 1) \\ B superblocks, where B is the logical size of a superblock and \\ is truncated division.

2 Allocate n superblocks.

3 Split the file into superblocks. For every superblock:

  3.1 Split the superblock into (m – 1) 512 KiB blocks, where m is the disk count on the pool. For every block:

      3.1.1 Split the block into up to 128 4K subblocks.

      3.1.2 Hash every subblock individually.

      3.1.3 Write all 128 data subblocks, followed by the one hash subblock, consecutively into the same physical disk.

  3.2 Before unloading the superblock from memory, generate the parity block. The parity block can be generated cumulatively by XORing each block in succession.

  3.3 The parity block must also be hashed analogously like the data blocks.

  3.4 Write the parity data block followed by its hash subblock into the parity disk.

4 When all the data has been committed to disk, update the database with file structure information and, crucially, per-file hash.

5 Subject to pool configuration, the file may be read again, to let the user know immediately if any data hasn’t been written properly. Depending on the result, the user will have different options available.

### Considerations

The order in which the subblocks are laid out within a superblock isn’t very important. The two main strategies are horizontally and vertically. Horizontally means that a file’s contents are laid out consecutively within a single disk until reaching the end of the block, and then on the next disk, and so on. Vertically means that the first subblock goes to the first disk, then the second subblock goes to the second disk, and so on until the m – 1 disk, then it loops back to the first.

Either one makes little difference because most superblocks will be completely filled with data. Only the last superblock of a file will be incompletely filled.

Perhaps the marginally best strategy is to fill horizontally and then fill the remainder of the superblock by repeating the superblock from the start.

## Read procedure

For every superblock of a file:

1 For every superblock of a file:

  1.1 Read the entire superblock into memory.

  1.2 For every VSSB***:**

      1.2.1 ***Check each subblock with its corresponding hash.**

      1.2.2 ***If all the hashes match, then the data read from the VSSB is correct and no further work is needed. At this point, the parity may be optionally checked (perhaps depending on pool configuration). Continue to the next VSSB.**

      1.2.3 ***If more than one of the hashes doesn’t match then the VSSB cannot be recovered. Reading the file has failed partially.**

      1.2.4 ***Otherwise, check the parity subblock with its hash. If the hash doesn’t match then the VSSB also cannot be recovered. Reading the file has failed partially.**

      1.2.5 ***The corrupted subblock can be reconstructed as the XOR of the uncorrupted subblocks and the parity subblock.**

  1.3 ***The logical superblock should be assembled by correctly reordering subblocks according to the chosen policy.**

2 ***After the file has been fully read, it must be validated with its whole-file hash, even if the subblock hashes were correct. A mismatch could indicate a synchronization error between the database and the disk contents. The database takes priority over the disks.**

## Delete procedure

The delete procedure involves simply removing the file information from the database. No data needs to be written to the disk.

# Fault tolerance

This system can tolerate failure of any single disk, to various extents. Several scenarios will be outlined below.

## Scenario 1

### Description

A disk has failed completely; it has stopped responding to commands and/or doesn’t even show up to the operating system.

### Result

Parity information has been lost (even if the lost disk is not the parity one). Any files that had any flaws on the remaining disks cannot be fully reconstructed, but the rest can still be recovered.

## Scenario 2

### Description

Same as scenario 1, but two or more disks have failed completely.

### Result

All data in the pool is irreparably lost, other than metadata. In a pinch, some pieces of data may be recovered, but usually it will be too little to be of use. Consider that in a 3 disk pool, 50% of all bits will have been lost.

## Scenario 3

### Description

The SQLite database is deleted or otherwise completely destroyed.

### Result

All data is lost. The database stores file pointers and metadata, so without it, the data on the disks is meaningless.

## Scenario 4

### Description

The SQLite database is corrupted in part.

### Result

Whatever has been corrupted may be irreparable.

## Scenario 5

### Description

In a 4-disk pool, disk 1 suffers surface damage from addresses 1,000,000 to 2,000,000, and disk 3 suffers surface damage from addresses 10,000,000 to 10,100,000. In other words, multiple, non-overlapping disk damage.

### Result

The pool can continue operating normally, without any data loss.

## Scenario 6

### Description

In a 4-disk pool, disk 1 suffers surface damage from addresses 1,000,000 to 10,050,000, and disk 3 suffers surface damage from addresses 10,000,000 to 10,100,000. In other words, multiple, non-overlapping disk damage.

### Result

Approximately speaking, addresses 10,050,000 to 10,100,000 will be corrupted and unrecoverable. If there’s any data stored at those positions, it will be lost, but the system can detect this situation, and the superblock/s can be marked so they won’t be used again.
