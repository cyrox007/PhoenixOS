ENTRY(_start)

PHDRS
{
    text PT_LOAD FLAGS(5);
    rodata PT_LOAD FLAGS(4);
    data PT_LOAD FLAGS(6);
}

SECTIONS
{
    . = 0x0000000044000000;

    .text ALIGN(0x1000) :
    {
        KEEP(*(.text._start))
        *(.text .text.*)
    } :text

    .rodata ALIGN(0x1000) :
    {
        *(.rodata .rodata.*)
        *(.eh_frame .eh_frame.*)
    } :rodata

    .data ALIGN(0x1000) :
    {
        *(.data .data.*)
        *(.got .got.*)
    } :data

    .bss ALIGN(0x1000) (NOLOAD) :
    {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /DISCARD/ :
    {
        *(.comment)
        *(.note*)
    }
}
