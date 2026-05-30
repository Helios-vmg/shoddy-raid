This is a command-line utility to manage multi-disk pools built out of disks
that no longer function flawlessly (e.g. due to surface errors). The resulting
pools are meant to be somewhat reliable. The intended interface will support
adding new files to the store as well as browse, read, and delete stored files.
Note that the system is not intended to work as a normal file system. As such,
operations such as updating a file will not be supported, except by deleting and
readding a file with the same name. See doc/design.fodt for a full explanation
of the motivation and use case.

I'm additionally using this project to test AI coding tools. Although the code
will mostly be written by LLMs, I will not be committing any code without
reviewing it.
