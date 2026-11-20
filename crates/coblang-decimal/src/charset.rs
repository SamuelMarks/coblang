//! Pure-Rust codepage, EBCDIC (`CP037`, `CP500`, `CP1047`, `CP1140`), Latin-1/9, `CP1252`,
//! `UTF-8`, and `UTF-16BE`/`UTF-16LE` character-set transcoding (`Milestone M17`).

use core::cmp::Ordering;

use coblang_common::{CoblangError, Result, TextEncoding};

/// Sentinel in 256-entry SBCS decode tables marking an unassigned single-byte codepoint (`CP1252`).
const UNMAPPED_SBCS: u16 = 0xFFFE;

/// Canonical IBM `CP037` (`CCSID 37`, US/Canada EBCDIC) byte-to-Unicode-BMP table.
const CP037_TO_BMP: [u16; 256] = [
    0x0000, 0x0001, 0x0002, 0x0003, 0x009C, 0x0009, 0x0086, 0x007F, 0x0097, 0x008D, 0x008E, 0x000B,
    0x000C, 0x000D, 0x000E, 0x000F, 0x0010, 0x0011, 0x0012, 0x0013, 0x009D, 0x0085, 0x0008, 0x0087,
    0x0018, 0x0019, 0x0092, 0x008F, 0x001C, 0x001D, 0x001E, 0x001F, 0x0080, 0x0081, 0x0082, 0x0083,
    0x0084, 0x000A, 0x0017, 0x001B, 0x0088, 0x0089, 0x008A, 0x008B, 0x008C, 0x0005, 0x0006, 0x0007,
    0x0090, 0x0091, 0x0016, 0x0093, 0x0094, 0x0095, 0x0096, 0x0004, 0x0098, 0x0099, 0x009A, 0x009B,
    0x0014, 0x0015, 0x009E, 0x001A, 0x0020, 0x00A0, 0x00E2, 0x00E4, 0x00E0, 0x00E1, 0x00E3, 0x00E5,
    0x00E7, 0x00F1, 0x00A2, 0x002E, 0x003C, 0x0028, 0x002B, 0x007C, 0x0026, 0x00E9, 0x00EA, 0x00EB,
    0x00E8, 0x00ED, 0x00EE, 0x00EF, 0x00EC, 0x00DF, 0x0021, 0x0024, 0x002A, 0x0029, 0x003B, 0x00AC,
    0x002D, 0x002F, 0x00C2, 0x00C4, 0x00C0, 0x00C1, 0x00C3, 0x00C5, 0x00C7, 0x00D1, 0x00A6, 0x002C,
    0x0025, 0x005F, 0x003E, 0x003F, 0x00F8, 0x00C9, 0x00CA, 0x00CB, 0x00C8, 0x00CD, 0x00CE, 0x00CF,
    0x00CC, 0x0060, 0x003A, 0x0023, 0x0040, 0x0027, 0x003D, 0x0022, 0x00D8, 0x0061, 0x0062, 0x0063,
    0x0064, 0x0065, 0x0066, 0x0067, 0x0068, 0x0069, 0x00AB, 0x00BB, 0x00F0, 0x00FD, 0x00FE, 0x00B1,
    0x00B0, 0x006A, 0x006B, 0x006C, 0x006D, 0x006E, 0x006F, 0x0070, 0x0071, 0x0072, 0x00AA, 0x00BA,
    0x00E6, 0x00B8, 0x00C6, 0x00A4, 0x00B5, 0x007E, 0x0073, 0x0074, 0x0075, 0x0076, 0x0077, 0x0078,
    0x0079, 0x007A, 0x00A1, 0x00BF, 0x00D0, 0x00DD, 0x00DE, 0x00AE, 0x005E, 0x00A3, 0x00A5, 0x00B7,
    0x00A9, 0x00A7, 0x00B6, 0x00BC, 0x00BD, 0x00BE, 0x005B, 0x005D, 0x00AF, 0x00A8, 0x00B4, 0x00D7,
    0x007B, 0x0041, 0x0042, 0x0043, 0x0044, 0x0045, 0x0046, 0x0047, 0x0048, 0x0049, 0x00AD, 0x00F4,
    0x00F6, 0x00F2, 0x00F3, 0x00F5, 0x007D, 0x004A, 0x004B, 0x004C, 0x004D, 0x004E, 0x004F, 0x0050,
    0x0051, 0x0052, 0x00B9, 0x00FB, 0x00FC, 0x00F9, 0x00FA, 0x00FF, 0x005C, 0x00F7, 0x0053, 0x0054,
    0x0055, 0x0056, 0x0057, 0x0058, 0x0059, 0x005A, 0x00B2, 0x00D4, 0x00D6, 0x00D2, 0x00D3, 0x00D5,
    0x0030, 0x0031, 0x0032, 0x0033, 0x0034, 0x0035, 0x0036, 0x0037, 0x0038, 0x0039, 0x00B3, 0x00DB,
    0x00DC, 0x00D9, 0x00DA, 0x009F,
];

/// Builds `CP1140` (`CCSID 1140`, US/Canada EBCDIC + Euro `€` at `0x9F`) at compile time.
const CP1140_TO_BMP: [u16; 256] = {
    let mut t = CP037_TO_BMP;
    t[0x9F] = 0x20AC;
    t
};

/// Builds `CP500` (`CCSID 500`, International EBCDIC) at compile time.
const CP500_TO_BMP: [u16; 256] = {
    let mut t = CP037_TO_BMP;
    t[0x4A] = 0x005B;
    t[0x4F] = 0x0021;
    t[0x5A] = 0x005D;
    t[0x5F] = 0x005E;
    t[0xB0] = 0x00A2;
    t[0xBA] = 0x00AC;
    t[0xBB] = 0x007C;
    t
};

/// Builds `CP1047` (`CCSID 1047`, z/OS UNIX Open Systems EBCDIC) at compile time.
const CP1047_TO_BMP: [u16; 256] = {
    let mut t = CP037_TO_BMP;
    t[0x15] = 0x000A;
    t[0x25] = 0x0085;
    t[0x5F] = 0x005E;
    t[0xAD] = 0x005B;
    t[0xB0] = 0x00AC;
    t[0xBA] = 0x00DD;
    t[0xBB] = 0x00A8;
    t[0xBD] = 0x005D;
    t
};

/// Bijective 256-byte `CP037` / `CP1140` EBCDIC-to-Latin-1 lookup table.
const CP037_TO_LATIN1: [u8; 256] = [
    0x00, 0x01, 0x02, 0x03, 0x9C, 0x09, 0x86, 0x7F, 0x97, 0x8D, 0x8E, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x9D, 0x85, 0x08, 0x87, 0x18, 0x19, 0x92, 0x8F, 0x1C, 0x1D, 0x1E, 0x1F,
    0x80, 0x81, 0x82, 0x83, 0x84, 0x0A, 0x17, 0x1B, 0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x05, 0x06, 0x07,
    0x90, 0x91, 0x16, 0x93, 0x94, 0x95, 0x96, 0x04, 0x98, 0x99, 0x9A, 0x9B, 0x14, 0x15, 0x9E, 0x1A,
    0x20, 0xA0, 0xE2, 0xE4, 0xE0, 0xE1, 0xE3, 0xE5, 0xE7, 0xF1, 0xA2, 0x2E, 0x3C, 0x28, 0x2B, 0x7C,
    0x26, 0xE9, 0xEA, 0xEB, 0xE8, 0xED, 0xEE, 0xEF, 0xEC, 0xDF, 0x21, 0x24, 0x2A, 0x29, 0x3B, 0xAC,
    0x2D, 0x2F, 0xC2, 0xC4, 0xC0, 0xC1, 0xC3, 0xC5, 0xC7, 0xD1, 0xA6, 0x2C, 0x25, 0x5F, 0x3E, 0x3F,
    0xF8, 0xC9, 0xCA, 0xCB, 0xC8, 0xCD, 0xCE, 0xCF, 0xCC, 0x60, 0x3A, 0x23, 0x40, 0x27, 0x3D, 0x22,
    0xD8, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0xAB, 0xBB, 0xF0, 0xFD, 0xFE, 0xB1,
    0xB0, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F, 0x70, 0x71, 0x72, 0xAA, 0xBA, 0xE6, 0xB8, 0xC6, 0xA4,
    0xB5, 0x7E, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0xA1, 0xBF, 0xD0, 0xDD, 0xDE, 0xAE,
    0x5E, 0xA3, 0xA5, 0xB7, 0xA9, 0xA7, 0xB6, 0xBC, 0xBD, 0xBE, 0x5B, 0x5D, 0xAF, 0xA8, 0xB4, 0xD7,
    0x7B, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0xAD, 0xF4, 0xF6, 0xF2, 0xF3, 0xF5,
    0x7D, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F, 0x50, 0x51, 0x52, 0xB9, 0xFB, 0xFC, 0xF9, 0xFA, 0xFF,
    0x5C, 0xF7, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0xB2, 0xD4, 0xD6, 0xD2, 0xD3, 0xD5,
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0xB3, 0xDB, 0xDC, 0xD9, 0xDA, 0x9F,
];

/// Bijective 256-byte `CP037` / `CP1140` Latin-1-to-EBCDIC lookup table.
const CP037_FROM_LATIN1: [u8; 256] = [
    0x00, 0x01, 0x02, 0x03, 0x37, 0x2D, 0x2E, 0x2F, 0x16, 0x05, 0x25, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x3C, 0x3D, 0x32, 0x26, 0x18, 0x19, 0x3F, 0x27, 0x1C, 0x1D, 0x1E, 0x1F,
    0x40, 0x5A, 0x7F, 0x7B, 0x5B, 0x6C, 0x50, 0x7D, 0x4D, 0x5D, 0x5C, 0x4E, 0x6B, 0x60, 0x4B, 0x61,
    0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0x7A, 0x5E, 0x4C, 0x7E, 0x6E, 0x6F,
    0x7C, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xD1, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6,
    0xD7, 0xD8, 0xD9, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xBA, 0xE0, 0xBB, 0xB0, 0x6D,
    0x79, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96,
    0x97, 0x98, 0x99, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xC0, 0x4F, 0xD0, 0xA1, 0x07,
    0x20, 0x21, 0x22, 0x23, 0x24, 0x15, 0x06, 0x17, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x09, 0x0A, 0x1B,
    0x30, 0x31, 0x1A, 0x33, 0x34, 0x35, 0x36, 0x08, 0x38, 0x39, 0x3A, 0x3B, 0x04, 0x14, 0x3E, 0xFF,
    0x41, 0xAA, 0x4A, 0xB1, 0x9F, 0xB2, 0x6A, 0xB5, 0xBD, 0xB4, 0x9A, 0x8A, 0x5F, 0xCA, 0xAF, 0xBC,
    0x90, 0x8F, 0xEA, 0xFA, 0xBE, 0xA0, 0xB6, 0xB3, 0x9D, 0xDA, 0x9B, 0x8B, 0xB7, 0xB8, 0xB9, 0xAB,
    0x64, 0x65, 0x62, 0x66, 0x63, 0x67, 0x9E, 0x68, 0x74, 0x71, 0x72, 0x73, 0x78, 0x75, 0x76, 0x77,
    0xAC, 0x69, 0xED, 0xEE, 0xEB, 0xEF, 0xEC, 0xBF, 0x80, 0xFD, 0xFE, 0xFB, 0xFC, 0xAD, 0xAE, 0x59,
    0x44, 0x45, 0x42, 0x46, 0x43, 0x47, 0x9C, 0x48, 0x54, 0x51, 0x52, 0x53, 0x58, 0x55, 0x56, 0x57,
    0x8C, 0x49, 0xCD, 0xCE, 0xCB, 0xCF, 0xCC, 0xE1, 0x70, 0xDD, 0xDE, 0xDB, 0xDC, 0x8D, 0x8E, 0xDF,
];

/// Bijective 256-byte `CP500` EBCDIC-to-Latin-1 lookup table.
const CP500_TO_LATIN1: [u8; 256] = [
    0x00, 0x01, 0x02, 0x03, 0x9C, 0x09, 0x86, 0x7F, 0x97, 0x8D, 0x8E, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x9D, 0x85, 0x08, 0x87, 0x18, 0x19, 0x92, 0x8F, 0x1C, 0x1D, 0x1E, 0x1F,
    0x80, 0x81, 0x82, 0x83, 0x84, 0x0A, 0x17, 0x1B, 0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x05, 0x06, 0x07,
    0x90, 0x91, 0x16, 0x93, 0x94, 0x95, 0x96, 0x04, 0x98, 0x99, 0x9A, 0x9B, 0x14, 0x15, 0x9E, 0x1A,
    0x20, 0xA0, 0xE2, 0xE4, 0xE0, 0xE1, 0xE3, 0xE5, 0xE7, 0xF1, 0x5B, 0x2E, 0x3C, 0x28, 0x2B, 0x21,
    0x26, 0xE9, 0xEA, 0xEB, 0xE8, 0xED, 0xEE, 0xEF, 0xEC, 0xDF, 0x5D, 0x24, 0x2A, 0x29, 0x3B, 0x5E,
    0x2D, 0x2F, 0xC2, 0xC4, 0xC0, 0xC1, 0xC3, 0xC5, 0xC7, 0xD1, 0xA6, 0x2C, 0x25, 0x5F, 0x3E, 0x3F,
    0xF8, 0xC9, 0xCA, 0xCB, 0xC8, 0xCD, 0xCE, 0xCF, 0xCC, 0x60, 0x3A, 0x23, 0x40, 0x27, 0x3D, 0x22,
    0xD8, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0xAB, 0xBB, 0xF0, 0xFD, 0xFE, 0xB1,
    0xB0, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F, 0x70, 0x71, 0x72, 0xAA, 0xBA, 0xE6, 0xB8, 0xC6, 0xA4,
    0xB5, 0x7E, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0xA1, 0xBF, 0xD0, 0xDD, 0xDE, 0xAE,
    0xA2, 0xA3, 0xA5, 0xB7, 0xA9, 0xA7, 0xB6, 0xBC, 0xBD, 0xBE, 0xAC, 0x7C, 0xAF, 0xA8, 0xB4, 0xD7,
    0x7B, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0xAD, 0xF4, 0xF6, 0xF2, 0xF3, 0xF5,
    0x7D, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F, 0x50, 0x51, 0x52, 0xB9, 0xFB, 0xFC, 0xF9, 0xFA, 0xFF,
    0x5C, 0xF7, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0xB2, 0xD4, 0xD6, 0xD2, 0xD3, 0xD5,
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0xB3, 0xDB, 0xDC, 0xD9, 0xDA, 0x9F,
];

/// Bijective 256-byte `CP500` Latin-1-to-EBCDIC lookup table.
const CP500_FROM_LATIN1: [u8; 256] = [
    0x00, 0x01, 0x02, 0x03, 0x37, 0x2D, 0x2E, 0x2F, 0x16, 0x05, 0x25, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x3C, 0x3D, 0x32, 0x26, 0x18, 0x19, 0x3F, 0x27, 0x1C, 0x1D, 0x1E, 0x1F,
    0x40, 0x4F, 0x7F, 0x7B, 0x5B, 0x6C, 0x50, 0x7D, 0x4D, 0x5D, 0x5C, 0x4E, 0x6B, 0x60, 0x4B, 0x61,
    0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0x7A, 0x5E, 0x4C, 0x7E, 0x6E, 0x6F,
    0x7C, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xD1, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6,
    0xD7, 0xD8, 0xD9, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0x4A, 0xE0, 0x5A, 0x5F, 0x6D,
    0x79, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96,
    0x97, 0x98, 0x99, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xC0, 0xBB, 0xD0, 0xA1, 0x07,
    0x20, 0x21, 0x22, 0x23, 0x24, 0x15, 0x06, 0x17, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x09, 0x0A, 0x1B,
    0x30, 0x31, 0x1A, 0x33, 0x34, 0x35, 0x36, 0x08, 0x38, 0x39, 0x3A, 0x3B, 0x04, 0x14, 0x3E, 0xFF,
    0x41, 0xAA, 0xB0, 0xB1, 0x9F, 0xB2, 0x6A, 0xB5, 0xBD, 0xB4, 0x9A, 0x8A, 0xBA, 0xCA, 0xAF, 0xBC,
    0x90, 0x8F, 0xEA, 0xFA, 0xBE, 0xA0, 0xB6, 0xB3, 0x9D, 0xDA, 0x9B, 0x8B, 0xB7, 0xB8, 0xB9, 0xAB,
    0x64, 0x65, 0x62, 0x66, 0x63, 0x67, 0x9E, 0x68, 0x74, 0x71, 0x72, 0x73, 0x78, 0x75, 0x76, 0x77,
    0xAC, 0x69, 0xED, 0xEE, 0xEB, 0xEF, 0xEC, 0xBF, 0x80, 0xFD, 0xFE, 0xFB, 0xFC, 0xAD, 0xAE, 0x59,
    0x44, 0x45, 0x42, 0x46, 0x43, 0x47, 0x9C, 0x48, 0x54, 0x51, 0x52, 0x53, 0x58, 0x55, 0x56, 0x57,
    0x8C, 0x49, 0xCD, 0xCE, 0xCB, 0xCF, 0xCC, 0xE1, 0x70, 0xDD, 0xDE, 0xDB, 0xDC, 0x8D, 0x8E, 0xDF,
];

/// Bijective 256-byte `CP1047` EBCDIC-to-Latin-1 lookup table.
const CP1047_TO_LATIN1: [u8; 256] = [
    0x00, 0x01, 0x02, 0x03, 0x9C, 0x09, 0x86, 0x7F, 0x97, 0x8D, 0x8E, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x9D, 0x0A, 0x08, 0x87, 0x18, 0x19, 0x92, 0x8F, 0x1C, 0x1D, 0x1E, 0x1F,
    0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x17, 0x1B, 0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x05, 0x06, 0x07,
    0x90, 0x91, 0x16, 0x93, 0x94, 0x95, 0x96, 0x04, 0x98, 0x99, 0x9A, 0x9B, 0x14, 0x15, 0x9E, 0x1A,
    0x20, 0xA0, 0xE2, 0xE4, 0xE0, 0xE1, 0xE3, 0xE5, 0xE7, 0xF1, 0xA2, 0x2E, 0x3C, 0x28, 0x2B, 0x7C,
    0x26, 0xE9, 0xEA, 0xEB, 0xE8, 0xED, 0xEE, 0xEF, 0xEC, 0xDF, 0x21, 0x24, 0x2A, 0x29, 0x3B, 0x5E,
    0x2D, 0x2F, 0xC2, 0xC4, 0xC0, 0xC1, 0xC3, 0xC5, 0xC7, 0xD1, 0xA6, 0x2C, 0x25, 0x5F, 0x3E, 0x3F,
    0xF8, 0xC9, 0xCA, 0xCB, 0xC8, 0xCD, 0xCE, 0xCF, 0xCC, 0x60, 0x3A, 0x23, 0x40, 0x27, 0x3D, 0x22,
    0xD8, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0xAB, 0xBB, 0xF0, 0xFD, 0xFE, 0xB1,
    0xB0, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F, 0x70, 0x71, 0x72, 0xAA, 0xBA, 0xE6, 0xB8, 0xC6, 0xA4,
    0xB5, 0x7E, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0xA1, 0xBF, 0xD0, 0x5B, 0xDE, 0xAE,
    0xAC, 0xA3, 0xA5, 0xB7, 0xA9, 0xA7, 0xB6, 0xBC, 0xBD, 0xBE, 0xDD, 0xA8, 0xAF, 0x5D, 0xB4, 0xD7,
    0x7B, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0xAD, 0xF4, 0xF6, 0xF2, 0xF3, 0xF5,
    0x7D, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F, 0x50, 0x51, 0x52, 0xB9, 0xFB, 0xFC, 0xF9, 0xFA, 0xFF,
    0x5C, 0xF7, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0xB2, 0xD4, 0xD6, 0xD2, 0xD3, 0xD5,
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0xB3, 0xDB, 0xDC, 0xD9, 0xDA, 0x9F,
];

/// Bijective 256-byte `CP1047` Latin-1-to-EBCDIC lookup table.
const CP1047_FROM_LATIN1: [u8; 256] = [
    0x00, 0x01, 0x02, 0x03, 0x37, 0x2D, 0x2E, 0x2F, 0x16, 0x05, 0x15, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x3C, 0x3D, 0x32, 0x26, 0x18, 0x19, 0x3F, 0x27, 0x1C, 0x1D, 0x1E, 0x1F,
    0x40, 0x5A, 0x7F, 0x7B, 0x5B, 0x6C, 0x50, 0x7D, 0x4D, 0x5D, 0x5C, 0x4E, 0x6B, 0x60, 0x4B, 0x61,
    0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0x7A, 0x5E, 0x4C, 0x7E, 0x6E, 0x6F,
    0x7C, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xD1, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6,
    0xD7, 0xD8, 0xD9, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xAD, 0xE0, 0xBD, 0x5F, 0x6D,
    0x79, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96,
    0x97, 0x98, 0x99, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xC0, 0x4F, 0xD0, 0xA1, 0x07,
    0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x06, 0x17, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x09, 0x0A, 0x1B,
    0x30, 0x31, 0x1A, 0x33, 0x34, 0x35, 0x36, 0x08, 0x38, 0x39, 0x3A, 0x3B, 0x04, 0x14, 0x3E, 0xFF,
    0x41, 0xAA, 0x4A, 0xB1, 0x9F, 0xB2, 0x6A, 0xB5, 0xBB, 0xB4, 0x9A, 0x8A, 0xB0, 0xCA, 0xAF, 0xBC,
    0x90, 0x8F, 0xEA, 0xFA, 0xBE, 0xA0, 0xB6, 0xB3, 0x9D, 0xDA, 0x9B, 0x8B, 0xB7, 0xB8, 0xB9, 0xAB,
    0x64, 0x65, 0x62, 0x66, 0x63, 0x67, 0x9E, 0x68, 0x74, 0x71, 0x72, 0x73, 0x78, 0x75, 0x76, 0x77,
    0xAC, 0x69, 0xED, 0xEE, 0xEB, 0xEF, 0xEC, 0xBF, 0x80, 0xFD, 0xFE, 0xFB, 0xFC, 0xBA, 0xAE, 0x59,
    0x44, 0x45, 0x42, 0x46, 0x43, 0x47, 0x9C, 0x48, 0x54, 0x51, 0x52, 0x53, 0x58, 0x55, 0x56, 0x57,
    0x8C, 0x49, 0xCD, 0xCE, 0xCB, 0xCF, 0xCC, 0xE1, 0x70, 0xDD, 0xDE, 0xDB, 0xDC, 0x8D, 0x8E, 0xDF,
];

/// Canonical `ISO-8859-1` (`Latin-1`) identity byte-to-BMP table.
const ISO8859_1_TO_BMP: [u16; 256] = [
    0x0000, 0x0001, 0x0002, 0x0003, 0x0004, 0x0005, 0x0006, 0x0007, 0x0008, 0x0009, 0x000A, 0x000B,
    0x000C, 0x000D, 0x000E, 0x000F, 0x0010, 0x0011, 0x0012, 0x0013, 0x0014, 0x0015, 0x0016, 0x0017,
    0x0018, 0x0019, 0x001A, 0x001B, 0x001C, 0x001D, 0x001E, 0x001F, 0x0020, 0x0021, 0x0022, 0x0023,
    0x0024, 0x0025, 0x0026, 0x0027, 0x0028, 0x0029, 0x002A, 0x002B, 0x002C, 0x002D, 0x002E, 0x002F,
    0x0030, 0x0031, 0x0032, 0x0033, 0x0034, 0x0035, 0x0036, 0x0037, 0x0038, 0x0039, 0x003A, 0x003B,
    0x003C, 0x003D, 0x003E, 0x003F, 0x0040, 0x0041, 0x0042, 0x0043, 0x0044, 0x0045, 0x0046, 0x0047,
    0x0048, 0x0049, 0x004A, 0x004B, 0x004C, 0x004D, 0x004E, 0x004F, 0x0050, 0x0051, 0x0052, 0x0053,
    0x0054, 0x0055, 0x0056, 0x0057, 0x0058, 0x0059, 0x005A, 0x005B, 0x005C, 0x005D, 0x005E, 0x005F,
    0x0060, 0x0061, 0x0062, 0x0063, 0x0064, 0x0065, 0x0066, 0x0067, 0x0068, 0x0069, 0x006A, 0x006B,
    0x006C, 0x006D, 0x006E, 0x006F, 0x0070, 0x0071, 0x0072, 0x0073, 0x0074, 0x0075, 0x0076, 0x0077,
    0x0078, 0x0079, 0x007A, 0x007B, 0x007C, 0x007D, 0x007E, 0x007F, 0x0080, 0x0081, 0x0082, 0x0083,
    0x0084, 0x0085, 0x0086, 0x0087, 0x0088, 0x0089, 0x008A, 0x008B, 0x008C, 0x008D, 0x008E, 0x008F,
    0x0090, 0x0091, 0x0092, 0x0093, 0x0094, 0x0095, 0x0096, 0x0097, 0x0098, 0x0099, 0x009A, 0x009B,
    0x009C, 0x009D, 0x009E, 0x009F, 0x00A0, 0x00A1, 0x00A2, 0x00A3, 0x00A4, 0x00A5, 0x00A6, 0x00A7,
    0x00A8, 0x00A9, 0x00AA, 0x00AB, 0x00AC, 0x00AD, 0x00AE, 0x00AF, 0x00B0, 0x00B1, 0x00B2, 0x00B3,
    0x00B4, 0x00B5, 0x00B6, 0x00B7, 0x00B8, 0x00B9, 0x00BA, 0x00BB, 0x00BC, 0x00BD, 0x00BE, 0x00BF,
    0x00C0, 0x00C1, 0x00C2, 0x00C3, 0x00C4, 0x00C5, 0x00C6, 0x00C7, 0x00C8, 0x00C9, 0x00CA, 0x00CB,
    0x00CC, 0x00CD, 0x00CE, 0x00CF, 0x00D0, 0x00D1, 0x00D2, 0x00D3, 0x00D4, 0x00D5, 0x00D6, 0x00D7,
    0x00D8, 0x00D9, 0x00DA, 0x00DB, 0x00DC, 0x00DD, 0x00DE, 0x00DF, 0x00E0, 0x00E1, 0x00E2, 0x00E3,
    0x00E4, 0x00E5, 0x00E6, 0x00E7, 0x00E8, 0x00E9, 0x00EA, 0x00EB, 0x00EC, 0x00ED, 0x00EE, 0x00EF,
    0x00F0, 0x00F1, 0x00F2, 0x00F3, 0x00F4, 0x00F5, 0x00F6, 0x00F7, 0x00F8, 0x00F9, 0x00FA, 0x00FB,
    0x00FC, 0x00FD, 0x00FE, 0x00FF,
];

/// Builds `ISO-8859-15` (`Latin-9`) byte-to-BMP table at compile time.
const ISO8859_15_TO_BMP: [u16; 256] = {
    let mut t = ISO8859_1_TO_BMP;
    t[0xA4] = 0x20AC;
    t[0xA6] = 0x0160;
    t[0xA8] = 0x0161;
    t[0xB4] = 0x017D;
    t[0xB8] = 0x017E;
    t[0xBC] = 0x0152;
    t[0xBD] = 0x0153;
    t[0xBE] = 0x0178;
    t
};

/// Canonical `CP1252` (`Windows-1252`) byte-to-BMP table (with 5 `UNMAPPED_SBCS` slots).
const CP1252_TO_BMP: [u16; 256] = {
    let mut t = ISO8859_1_TO_BMP;
    t[0x80] = 0x20AC;
    t[0x81] = UNMAPPED_SBCS;
    t[0x82] = 0x201A;
    t[0x83] = 0x0192;
    t[0x84] = 0x201E;
    t[0x85] = 0x2026;
    t[0x86] = 0x2020;
    t[0x87] = 0x2021;
    t[0x88] = 0x02C6;
    t[0x89] = 0x2030;
    t[0x8A] = 0x0160;
    t[0x8B] = 0x2039;
    t[0x8C] = 0x0152;
    t[0x8D] = UNMAPPED_SBCS;
    t[0x8E] = 0x017D;
    t[0x8F] = UNMAPPED_SBCS;
    t[0x90] = UNMAPPED_SBCS;
    t[0x91] = 0x2018;
    t[0x92] = 0x2019;
    t[0x93] = 0x201C;
    t[0x94] = 0x201D;
    t[0x95] = 0x2022;
    t[0x96] = 0x2013;
    t[0x97] = 0x2014;
    t[0x98] = 0x02DC;
    t[0x99] = 0x2122;
    t[0x9A] = 0x0161;
    t[0x9B] = 0x203A;
    t[0x9C] = 0x0153;
    t[0x9D] = UNMAPPED_SBCS;
    t[0x9E] = 0x017E;
    t[0x9F] = 0x0178;
    t
};

/// Supported pure-Rust source and runtime COBOL character encodings (`Milestone M17`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codepage {
    /// IBM EBCDIC US/Canada + Euro (`CCSID 1140`; default `-finternal-ebcdic` codepage).
    Cp1140,
    /// IBM EBCDIC US/Canada (`CCSID 37`).
    Cp037,
    /// IBM EBCDIC International (`CCSID 500`).
    Cp500,
    /// IBM EBCDIC z/OS Open Systems (`CCSID 1047`).
    Cp1047,
    /// `ISO-8859-1` (`Latin-1` / raw 8-bit ASCII extension).
    Iso8859_1,
    /// `ISO-8859-15` (`Latin-9` with Euro `€`).
    Iso8859_15,
    /// `CP1252` (`Windows-1252` Western European).
    Cp1252,
    /// Variable-width `UTF-8` (`PIC U`).
    Utf8,
    /// Big-endian `UTF-16BE` (`PIC N` canonical `USAGE NATIONAL` representation).
    Utf16Be,
    /// Little-endian `UTF-16LE`.
    Utf16Le,
}

/// Ordered slice of all 10 supported [`Codepage`] variants.
pub const ALL_CODEPAGES: [Codepage; 10] = [
    Codepage::Cp1140,
    Codepage::Cp037,
    Codepage::Cp500,
    Codepage::Cp1047,
    Codepage::Iso8859_1,
    Codepage::Iso8859_15,
    Codepage::Cp1252,
    Codepage::Utf8,
    Codepage::Utf16Be,
    Codepage::Utf16Le,
];

impl Codepage {
    /// Returns all 10 supported [`Codepage`] variants.
    #[must_use]
    pub const fn all() -> &'static [Self; 10] {
        &ALL_CODEPAGES
    }

    /// Returns the canonical IANA/IBM codepage identifier string.
    #[must_use]
    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::Cp1140 => "CP1140",
            Self::Cp037 => "CP037",
            Self::Cp500 => "CP500",
            Self::Cp1047 => "CP1047",
            Self::Iso8859_1 => "ISO-8859-1",
            Self::Iso8859_15 => "ISO-8859-15",
            Self::Cp1252 => "CP1252",
            Self::Utf8 => "UTF-8",
            Self::Utf16Be => "UTF-16BE",
            Self::Utf16Le => "UTF-16LE",
        }
    }

    /// Converts `self` to a [`TextEncoding`] value for diagnostics and manifest tags.
    #[must_use]
    pub fn to_text_encoding(self) -> TextEncoding {
        match self {
            Self::Iso8859_1 => TextEncoding::Ascii,
            Self::Cp1140 => TextEncoding::Ebcdic,
            Self::Cp037
            | Self::Cp500
            | Self::Cp1047
            | Self::Iso8859_15
            | Self::Cp1252
            | Self::Utf8
            | Self::Utf16Be
            | Self::Utf16Le => TextEncoding::Codepage(String::from(self.canonical_name())),
        }
    }

    /// Parses a case-insensitive codepage alias (`"CP1140"`, `"IBM037"`, `"LATIN9"`, `"UTF-16BE"`, etc.).
    ///
    /// # Arguments
    ///
    /// * `raw` - Codepage name or alias (optionally carrying `//TRANSLIT` or `//IGNORE` suffixes).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw` is unrecognized.
    pub fn from_name(raw: &str) -> Result<Self> {
        let norm = normalize_codepage_token(raw);
        match norm.as_str() {
            "CP1140" | "CP01140" | "IBM1140" | "IBM01140" | "CCSID1140" | "CCSID01140"
            | "EBCDIC" => Ok(Self::Cp1140),
            "CP037" | "CP37" | "CP0037" | "IBM037" | "IBM37" | "CCSID37" | "CCSID00037"
            | "CSIBM037" | "EBCDICUS" | "EBCDICCPUS" | "GC037" => Ok(Self::Cp037),
            "CP500"
            | "CP0500"
            | "IBM500"
            | "IBM0500"
            | "CCSID500"
            | "CCSID00500"
            | "CSIBM500"
            | "EBCDIC500"
            | "EBCDIC500LATIN1"
            | "EBCDICINTERNATIONAL" => Ok(Self::Cp500),
            "CP1047" | "IBM1047" | "IBM01047" | "CCSID1047" | "CCSID01047" | "EBCDIC1047" => {
                Ok(Self::Cp1047)
            }
            "ISO88591" | "ISOIR100" | "LATIN1" | "ASCII" | "USASCII" | "ANSIX341968" => {
                Ok(Self::Iso8859_1)
            }
            "ISO885915" | "ISOIR203" | "LATIN9" => Ok(Self::Iso8859_15),
            "CP1252" | "WINDOWS1252" | "WIN1252" | "MS1252" => Ok(Self::Cp1252),
            "UTF8" => Ok(Self::Utf8),
            "UTF16BE" | "UTF16" | "UCS2BE" | "NATIONAL" => Ok(Self::Utf16Be),
            "UTF16LE" | "UCS2LE" => Ok(Self::Utf16Le),
            _ => Err(CoblangError::InvalidDomainValue {
                domain_type: "Codepage",
                value: String::from(raw),
                reason: String::from("unsupported codepage identifier"),
            }),
        }
    }

    /// Resolves a [`TextEncoding`] configuration into its concrete [`Codepage`].
    ///
    /// # Arguments
    ///
    /// * `encoding` - Compiler/runtime [`TextEncoding`] selector.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `TextEncoding::Codepage` names an unknown codepage.
    pub fn from_text_encoding(encoding: &TextEncoding) -> Result<Self> {
        match encoding {
            TextEncoding::Ascii => Ok(Self::Iso8859_1),
            TextEncoding::Ebcdic => Ok(Self::Cp1140),
            TextEncoding::Codepage(name) => Self::from_name(name),
        }
    }

    /// Returns `true` for the four EBCDIC codepages (`CP1140`, `CP037`, `CP500`, `CP1047`).
    #[must_use]
    pub const fn is_ebcdic(self) -> bool {
        matches!(
            self,
            Self::Cp1140 | Self::Cp037 | Self::Cp500 | Self::Cp1047
        )
    }

    /// Returns the single-byte `SPACE` codepoint (`0x40` for EBCDIC, `0x20` for ASCII/UTF-8, `None` for UTF-16).
    #[must_use]
    pub const fn single_byte_space(self) -> Option<u8> {
        match self {
            Self::Cp1140 | Self::Cp037 | Self::Cp500 | Self::Cp1047 => Some(0x40),
            Self::Iso8859_1 | Self::Iso8859_15 | Self::Cp1252 | Self::Utf8 => Some(0x20),
            Self::Utf16Be | Self::Utf16Le => None,
        }
    }

    /// Returns the encoded [`FigurativeUnit`] for `fig` in `self`.
    ///
    /// # Arguments
    ///
    /// * `fig` - COBOL [`FigurativeConstantChar`] selector (`SPACE`, `ZERO`, `QUOTE`, `LOW-VALUE`, `HIGH-VALUE`).
    #[must_use]
    pub const fn figurative_unit(self, fig: FigurativeConstantChar) -> FigurativeUnit {
        match self {
            Self::Cp1140 | Self::Cp037 | Self::Cp500 | Self::Cp1047 => match fig {
                FigurativeConstantChar::Space => FigurativeUnit::SingleByte(0x40),
                FigurativeConstantChar::Zero => FigurativeUnit::SingleByte(0xF0),
                FigurativeConstantChar::QuoteDouble => FigurativeUnit::SingleByte(0x7F),
                FigurativeConstantChar::QuoteApostrophe => FigurativeUnit::SingleByte(0x7D),
                FigurativeConstantChar::LowValue => FigurativeUnit::SingleByte(0x00),
                FigurativeConstantChar::HighValue => FigurativeUnit::SingleByte(0xFF),
            },
            Self::Iso8859_1 | Self::Iso8859_15 | Self::Cp1252 | Self::Utf8 => match fig {
                FigurativeConstantChar::Space => FigurativeUnit::SingleByte(0x20),
                FigurativeConstantChar::Zero => FigurativeUnit::SingleByte(0x30),
                FigurativeConstantChar::QuoteDouble => FigurativeUnit::SingleByte(0x22),
                FigurativeConstantChar::QuoteApostrophe => FigurativeUnit::SingleByte(0x27),
                FigurativeConstantChar::LowValue => FigurativeUnit::SingleByte(0x00),
                FigurativeConstantChar::HighValue => FigurativeUnit::SingleByte(0xFF),
            },
            Self::Utf16Be => match fig {
                FigurativeConstantChar::Space => FigurativeUnit::DoubleByte([0x00, 0x20]),
                FigurativeConstantChar::Zero => FigurativeUnit::DoubleByte([0x00, 0x30]),
                FigurativeConstantChar::QuoteDouble => FigurativeUnit::DoubleByte([0x00, 0x22]),
                FigurativeConstantChar::QuoteApostrophe => FigurativeUnit::DoubleByte([0x00, 0x27]),
                FigurativeConstantChar::LowValue => FigurativeUnit::DoubleByte([0x00, 0x00]),
                FigurativeConstantChar::HighValue => FigurativeUnit::DoubleByte([0xFF, 0xFF]),
            },
            Self::Utf16Le => match fig {
                FigurativeConstantChar::Space => FigurativeUnit::DoubleByte([0x20, 0x00]),
                FigurativeConstantChar::Zero => FigurativeUnit::DoubleByte([0x30, 0x00]),
                FigurativeConstantChar::QuoteDouble => FigurativeUnit::DoubleByte([0x22, 0x00]),
                FigurativeConstantChar::QuoteApostrophe => FigurativeUnit::DoubleByte([0x27, 0x00]),
                FigurativeConstantChar::LowValue => FigurativeUnit::DoubleByte([0x00, 0x00]),
                FigurativeConstantChar::HighValue => FigurativeUnit::DoubleByte([0xFF, 0xFF]),
            },
        }
    }

    /// Returns the internal [`CodepageRepr`] backing `self`.
    #[must_use]
    const fn repr(self) -> CodepageRepr {
        match self {
            Self::Cp1140 => CodepageRepr::Sbcs(&CP1140_TO_BMP),
            Self::Cp037 => CodepageRepr::Sbcs(&CP037_TO_BMP),
            Self::Cp500 => CodepageRepr::Sbcs(&CP500_TO_BMP),
            Self::Cp1047 => CodepageRepr::Sbcs(&CP1047_TO_BMP),
            Self::Iso8859_1 => CodepageRepr::Sbcs(&ISO8859_1_TO_BMP),
            Self::Iso8859_15 => CodepageRepr::Sbcs(&ISO8859_15_TO_BMP),
            Self::Cp1252 => CodepageRepr::Sbcs(&CP1252_TO_BMP),
            Self::Utf8 => CodepageRepr::Utf8,
            Self::Utf16Be => CodepageRepr::Utf16(Utf16Endian::Big),
            Self::Utf16Le => CodepageRepr::Utf16(Utf16Endian::Little),
        }
    }
}

/// Normalizes a codepage or EBCDIC table alias by stripping `//` suffixes, `-`/`_`, and uppercasing.
///
/// # Arguments
///
/// * `raw` - Raw codepage or table name token.
#[must_use]
fn normalize_codepage_token(raw: &str) -> String {
    let base = raw.trim().split("//").next().unwrap_or("");
    let mut norm = String::with_capacity(base.len());
    for b in base.bytes() {
        if b != b'-' && b != b'_' {
            norm.push(char::from(b.to_ascii_uppercase()));
        }
    }
    norm
}

/// Strongly-typed EBCDIC single-byte codepage family (`CP1140`, `CP037`, `CP500`, `CP1047`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EbcdicCodepage {
    /// IBM EBCDIC US/Canada + Euro (`CCSID 1140`; default `-finternal-ebcdic` & `-febcdic-table=default`).
    Cp1140,
    /// IBM EBCDIC US/Canada (`CCSID 37`; `-febcdic-table=gc037` / `ibm`).
    Cp037,
    /// IBM EBCDIC International (`CCSID 500`; `-febcdic-table=ebcdic500_latin1` / `restricted-gc`).
    Cp500,
    /// IBM EBCDIC z/OS Open Systems (`CCSID 1047`).
    Cp1047,
}

/// Ordered slice of all four supported [`EbcdicCodepage`] variants.
pub const ALL_EBCDIC_CODEPAGES: [EbcdicCodepage; 4] = [
    EbcdicCodepage::Cp1140,
    EbcdicCodepage::Cp037,
    EbcdicCodepage::Cp500,
    EbcdicCodepage::Cp1047,
];

impl EbcdicCodepage {
    /// Returns all four supported [`EbcdicCodepage`] variants.
    #[must_use]
    pub const fn all() -> &'static [Self; 4] {
        &ALL_EBCDIC_CODEPAGES
    }

    /// Converts `self` into its corresponding general [`Codepage`] variant.
    #[must_use]
    pub const fn to_codepage(self) -> Codepage {
        match self {
            Self::Cp1140 => Codepage::Cp1140,
            Self::Cp037 => Codepage::Cp037,
            Self::Cp500 => Codepage::Cp500,
            Self::Cp1047 => Codepage::Cp1047,
        }
    }

    /// Extracts the [`EbcdicCodepage`] from `codepage` when `codepage.is_ebcdic()` holds.
    ///
    /// # Arguments
    ///
    /// * `codepage` - General [`Codepage`] value.
    #[must_use]
    pub const fn try_from_codepage(codepage: Codepage) -> Option<Self> {
        match codepage {
            Codepage::Cp1140 => Some(Self::Cp1140),
            Codepage::Cp037 => Some(Self::Cp037),
            Codepage::Cp500 => Some(Self::Cp500),
            Codepage::Cp1047 => Some(Self::Cp1047),
            Codepage::Iso8859_1
            | Codepage::Iso8859_15
            | Codepage::Cp1252
            | Codepage::Utf8
            | Codepage::Utf16Be
            | Codepage::Utf16Le => None,
        }
    }

    /// Parses a `-febcdic-table=<name>` or EBCDIC codepage identifier (`"default"`, `"ibm"`, `"CP1140"`, etc.).
    ///
    /// # Arguments
    ///
    /// * `raw` - EBCDIC table preset or codepage alias.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw` does not identify a supported EBCDIC table.
    pub fn from_ebcdic_table_name(raw: &str) -> Result<Self> {
        let norm = normalize_codepage_token(raw);
        match norm.as_str() {
            "DEFAULT" | "CP1140" | "CP01140" | "IBM1140" | "IBM01140" | "CCSID1140"
            | "CCSID01140" | "EBCDIC" => Ok(Self::Cp1140),
            "IBM" | "GC037" | "CP037" | "CP37" | "CP0037" | "IBM037" | "IBM37" | "CCSID37"
            | "CCSID00037" | "CSIBM037" | "EBCDICUS" | "EBCDICCPUS" => Ok(Self::Cp037),
            "RESTRICTEDGC"
            | "EBCDIC500"
            | "EBCDIC500LATIN1"
            | "EBCDICINTERNATIONAL"
            | "CP500"
            | "CP0500"
            | "IBM500"
            | "IBM0500"
            | "CCSID500"
            | "CCSID00500"
            | "CSIBM500" => Ok(Self::Cp500),
            "EBCDIC1047" | "CP1047" | "IBM1047" | "IBM01047" | "CCSID1047" | "CCSID01047" => {
                Ok(Self::Cp1047)
            }
            _ => Err(CoblangError::InvalidDomainValue {
                domain_type: "EbcdicCodepage",
                value: String::from(raw),
                reason: String::from("unsupported EBCDIC table identifier"),
            }),
        }
    }

    /// Maps a single ASCII / `ISO-8859-1` byte into its bijective EBCDIC byte in `O(1)` time.
    ///
    /// # Arguments
    ///
    /// * `ascii_byte` - Input ASCII / Latin-1 byte (`0x00..=0xFF`).
    #[must_use]
    pub fn ascii_to_ebcdic_byte(self, ascii_byte: u8) -> u8 {
        let table = match self {
            Self::Cp1140 | Self::Cp037 => &CP037_FROM_LATIN1,
            Self::Cp500 => &CP500_FROM_LATIN1,
            Self::Cp1047 => &CP1047_FROM_LATIN1,
        };
        table
            .get(usize::from(ascii_byte))
            .copied()
            .unwrap_or(ascii_byte)
    }

    /// Maps a single EBCDIC byte into its bijective ASCII / `ISO-8859-1` byte in `O(1)` time.
    ///
    /// # Arguments
    ///
    /// * `ebcdic_byte` - Input EBCDIC byte (`0x00..=0xFF`).
    #[must_use]
    pub fn ebcdic_to_ascii_byte(self, ebcdic_byte: u8) -> u8 {
        let table = match self {
            Self::Cp1140 | Self::Cp037 => &CP037_TO_LATIN1,
            Self::Cp500 => &CP500_TO_LATIN1,
            Self::Cp1047 => &CP1047_TO_LATIN1,
        };
        table
            .get(usize::from(ebcdic_byte))
            .copied()
            .unwrap_or(ebcdic_byte)
    }

    /// Transcodes an ASCII / `ISO-8859-1` byte slice into EBCDIC bytes using the `O(1)` bijective table.
    ///
    /// # Arguments
    ///
    /// * `src` - Source ASCII / Latin-1 bytes.
    #[must_use]
    pub fn ascii_to_ebcdic_bytes(self, src: &[u8]) -> Vec<u8> {
        src.iter().map(|&b| self.ascii_to_ebcdic_byte(b)).collect()
    }

    /// Transcodes an EBCDIC byte slice into ASCII / `ISO-8859-1` bytes using the `O(1)` bijective table.
    ///
    /// # Arguments
    ///
    /// * `src` - Source EBCDIC bytes.
    #[must_use]
    pub fn ebcdic_to_ascii_bytes(self, src: &[u8]) -> Vec<u8> {
        src.iter().map(|&b| self.ebcdic_to_ascii_byte(b)).collect()
    }

    /// Computes the ISO 2023 1-based `FUNCTION ORD` ordinal (`1..=256`) of `ascii_byte` under EBCDIC collation.
    ///
    /// # Arguments
    ///
    /// * `ascii_byte` - Single-byte character encoded in ASCII / Latin-1.
    #[must_use]
    pub fn ord_1based_of_ascii(self, ascii_byte: u8) -> u16 {
        u16::from(self.ascii_to_ebcdic_byte(ascii_byte)).saturating_add(1)
    }

    /// Computes the ASCII / Latin-1 byte for ISO 2023 1-based `FUNCTION CHAR(ord_1based)` under EBCDIC collation.
    ///
    /// # Arguments
    ///
    /// * `ord_1based` - 1-based ordinal position in `1..=256`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `ord_1based` is outside `1..=256`.
    pub fn ascii_char_from_ord_1based(self, ord_1based: u16) -> Result<u8> {
        if !(1..=256).contains(&ord_1based) {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "EbcdicOrdinal",
                value: ord_1based.to_string(),
                reason: String::from("COBOL ORD/CHAR ordinal must be in 1..=256"),
            });
        }
        let ebcdic_byte = u8::try_from(ord_1based - 1).unwrap_or(0);
        Ok(self.ebcdic_to_ascii_byte(ebcdic_byte))
    }
}

/// COBOL figurative character selector for character-set-aware padding and initialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FigurativeConstantChar {
    /// `SPACE` / `SPACES` (`0x40` EBCDIC, `0x20` ASCII/UTF-8, `U+0020` UTF-16).
    Space,
    /// `ZERO` / `ZEROS` / `ZEROES` (`0xF0` EBCDIC, `0x30` ASCII/UTF-8, `U+0030` UTF-16).
    Zero,
    /// `QUOTE` / `QUOTES` in double-quote mode (`0x7F` EBCDIC, `0x22` ASCII/UTF-8, `U+0022` UTF-16).
    QuoteDouble,
    /// `QUOTE` / `QUOTES` in `APOST` mode (`0x7D` EBCDIC, `0x27` ASCII/UTF-8, `U+0027` UTF-16).
    QuoteApostrophe,
    /// `LOW-VALUE` / `LOW-VALUES` (`0x00` SBCS/UTF-8, `U+0000` UTF-16).
    LowValue,
    /// `HIGH-VALUE` / `HIGH-VALUES` (`0xFF` SBCS/UTF-8, `U+FFFF` UTF-16).
    HighValue,
}

/// Ordered slice of all six [`FigurativeConstantChar`] variants.
pub const ALL_FIGURATIVE_CHARS: [FigurativeConstantChar; 6] = [
    FigurativeConstantChar::Space,
    FigurativeConstantChar::Zero,
    FigurativeConstantChar::QuoteDouble,
    FigurativeConstantChar::QuoteApostrophe,
    FigurativeConstantChar::LowValue,
    FigurativeConstantChar::HighValue,
];

/// Encoded single-unit byte representation of a [`FigurativeConstantChar`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FigurativeUnit {
    /// Single-byte figurative unit (EBCDIC, ISO-8859, `CP1252`, `UTF-8`).
    SingleByte(u8),
    /// Two-byte figurative unit (`UTF-16BE` or `UTF-16LE`).
    DoubleByte([u8; 2]),
}

impl FigurativeUnit {
    /// Returns the borrowed byte slice (`1` or `2` bytes) for `self`.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8] {
        match self {
            Self::SingleByte(b) => core::slice::from_ref(b),
            Self::DoubleByte(pair) => pair.as_slice(),
        }
    }
}

/// Internal storage/codec representation of a [`Codepage`].
#[derive(Clone, Copy)]
enum CodepageRepr {
    /// 256-entry single-byte BMP lookup table.
    Sbcs(&'static [u16; 256]),
    /// Variable-width UTF-8.
    Utf8,
    /// Fixed-unit 16-bit UTF-16 (`Big` or `Little`).
    Utf16(Utf16Endian),
}

/// Disposition policy for malformed byte sequences or unrepresentable target codepoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnmappedCharPolicy {
    /// Fail immediately with [`CoblangError::EncodingConversion`].
    StrictError,
    /// Substitute `char` (falling back to `'?'` if `char` itself is unrepresentable in target SBCS).
    Substitute(char),
}

/// Justification mode for `PIC N` (`NATIONAL` UTF-16BE) `MOVE` padding and truncation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NationalJustify {
    /// Standard left-justified `PIC N`: right-pad with `U+0020` or truncate excess right units.
    Left,
    /// `JUSTIFIED RIGHT` `PIC N`: left-pad with `U+0020` or truncate excess left units.
    Right,
}

/// Decodes `src` bytes from `codepage` into Unicode scalar values (`Vec<char>`).
///
/// # Arguments
///
/// * `src` - Input encoded byte slice.
/// * `codepage` - Source [`Codepage`].
/// * `policy` - Error vs substitution behavior on invalid bytes/surrogates.
///
/// # Errors
///
/// Returns [`CoblangError::EncodingConversion`] under [`UnmappedCharPolicy::StrictError`] when `src` contains invalid bytes.
pub fn decode_to_chars(
    src: &[u8],
    codepage: Codepage,
    policy: UnmappedCharPolicy,
) -> Result<Vec<char>> {
    match codepage.repr() {
        CodepageRepr::Sbcs(table) => decode_sbcs(src, table, codepage, policy),
        CodepageRepr::Utf8 => decode_utf8(src, policy),
        CodepageRepr::Utf16(endian) => decode_utf16(src, endian, codepage, policy),
    }
}

/// Byte-order selector for UTF-16 decoding/encoding helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Utf16Endian {
    /// Big-endian (`UTF-16BE`).
    Big,
    /// Little-endian (`UTF-16LE`).
    Little,
}

/// Decodes single-byte `src` using `table`.
///
/// # Arguments
///
/// * `src` - Input SBCS bytes.
/// * `table` - 256-entry BMP codepoint table.
/// * `codepage` - Source [`Codepage`] for error reporting.
/// * `policy` - [`UnmappedCharPolicy`].
fn decode_sbcs(
    src: &[u8],
    table: &[u16; 256],
    codepage: Codepage,
    policy: UnmappedCharPolicy,
) -> Result<Vec<char>> {
    let mut out = Vec::with_capacity(src.len());
    for (idx, &b) in src.iter().enumerate() {
        let bmp = table.get(usize::from(b)).copied().unwrap_or(UNMAPPED_SBCS);
        if bmp == UNMAPPED_SBCS {
            match policy {
                UnmappedCharPolicy::StrictError => {
                    return Err(CoblangError::EncodingConversion {
                        encoding: codepage.to_text_encoding(),
                        byte_offset: idx,
                        message: format!(
                            "unassigned byte 0x{b:02X} in {}",
                            codepage.canonical_name()
                        ),
                    });
                }
                UnmappedCharPolicy::Substitute(rep) => out.push(rep),
            }
        } else {
            out.push(char::from_u32(u32::from(bmp)).unwrap_or('\u{FFFD}'));
        }
    }
    Ok(out)
}

/// Decodes UTF-8 `src` with strict error or per-chunk substitution recovery.
///
/// # Arguments
///
/// * `src` - Input UTF-8 bytes.
/// * `policy` - [`UnmappedCharPolicy`].
fn decode_utf8(src: &[u8], policy: UnmappedCharPolicy) -> Result<Vec<char>> {
    let mut out = Vec::with_capacity(src.len());
    let mut offset = 0_usize;
    for chunk in src.utf8_chunks() {
        let valid = chunk.valid();
        out.extend(valid.chars());
        offset = offset.saturating_add(valid.len());
        let invalid = chunk.invalid();
        if !invalid.is_empty() {
            match policy {
                UnmappedCharPolicy::StrictError => {
                    return Err(CoblangError::EncodingConversion {
                        encoding: Codepage::Utf8.to_text_encoding(),
                        byte_offset: offset,
                        message: String::from("invalid UTF-8 byte sequence"),
                    });
                }
                UnmappedCharPolicy::Substitute(rep) => {
                    out.push(rep);
                    offset = offset.saturating_add(invalid.len());
                }
            }
        }
    }
    Ok(out)
}

/// Decodes `UTF-16BE` or `UTF-16LE` bytes into Unicode scalar values.
///
/// # Arguments
///
/// * `src` - Input UTF-16 byte slice.
/// * `endian` - [`Utf16Endian`] byte order.
/// * `codepage` - [`Codepage`] tag for error diagnostics.
/// * `policy` - [`UnmappedCharPolicy`].
fn decode_utf16(
    src: &[u8],
    endian: Utf16Endian,
    codepage: Codepage,
    policy: UnmappedCharPolicy,
) -> Result<Vec<char>> {
    let (pairs, rem) = src.as_chunks::<2>();
    let mut units = Vec::with_capacity(pairs.len());
    for &[b0, b1] in pairs {
        let unit = match endian {
            Utf16Endian::Big => u16::from_be_bytes([b0, b1]),
            Utf16Endian::Little => u16::from_le_bytes([b0, b1]),
        };
        units.push(unit);
    }
    let mut out = Vec::with_capacity(units.len());
    for (unit_idx, decoded) in char::decode_utf16(units).enumerate() {
        match decoded {
            Ok(ch) => out.push(ch),
            Err(_) => match policy {
                UnmappedCharPolicy::StrictError => {
                    return Err(CoblangError::EncodingConversion {
                        encoding: codepage.to_text_encoding(),
                        byte_offset: unit_idx.saturating_mul(2),
                        message: String::from("unpaired UTF-16 surrogate code unit"),
                    });
                }
                UnmappedCharPolicy::Substitute(rep) => out.push(rep),
            },
        }
    }
    if !rem.is_empty() {
        match policy {
            UnmappedCharPolicy::StrictError => {
                return Err(CoblangError::EncodingConversion {
                    encoding: codepage.to_text_encoding(),
                    byte_offset: src.len().saturating_sub(1),
                    message: String::from("trailing odd byte in UTF-16 buffer"),
                });
            }
            UnmappedCharPolicy::Substitute(rep) => out.push(rep),
        }
    }
    Ok(out)
}

/// Encodes Unicode scalar values (`chars`) into `codepage` bytes.
///
/// # Arguments
///
/// * `chars` - Input Unicode scalar slice.
/// * `codepage` - Target [`Codepage`].
/// * `policy` - Error vs substitution behavior when a character cannot be represented in target SBCS.
///
/// # Errors
///
/// Returns [`CoblangError::EncodingConversion`] under [`UnmappedCharPolicy::StrictError`] if a character is unmapped.
pub fn encode_from_chars(
    chars: &[char],
    codepage: Codepage,
    policy: UnmappedCharPolicy,
) -> Result<Vec<u8>> {
    match codepage.repr() {
        CodepageRepr::Sbcs(table) => {
            let qmark_byte = lookup_sbcs_char('?', table).unwrap_or(b'?');
            let mut out = Vec::with_capacity(chars.len());
            for (idx, &ch) in chars.iter().enumerate() {
                if let Some(b) = lookup_sbcs_char(ch, table) {
                    out.push(b);
                } else {
                    match policy {
                        UnmappedCharPolicy::StrictError => {
                            return Err(CoblangError::EncodingConversion {
                                encoding: codepage.to_text_encoding(),
                                byte_offset: idx,
                                message: format!(
                                    "U+{:04X} is unrepresentable in {}",
                                    u32::from(ch),
                                    codepage.canonical_name()
                                ),
                            });
                        }
                        UnmappedCharPolicy::Substitute(rep) => {
                            out.push(lookup_sbcs_char(rep, table).unwrap_or(qmark_byte));
                        }
                    }
                }
            }
            Ok(out)
        }
        CodepageRepr::Utf8 => {
            let mut out = Vec::with_capacity(chars.len());
            let mut buf = [0_u8; 4];
            for &ch in chars {
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
            Ok(out)
        }
        CodepageRepr::Utf16(endian) => {
            let mut out = Vec::with_capacity(chars.len().saturating_mul(2));
            let mut buf = [0_u16; 2];
            for &ch in chars {
                for &u in ch.encode_utf16(&mut buf).iter() {
                    let bytes = match endian {
                        Utf16Endian::Big => u.to_be_bytes(),
                        Utf16Endian::Little => u.to_le_bytes(),
                    };
                    out.extend_from_slice(&bytes);
                }
            }
            Ok(out)
        }
    }
}

/// Searches `table` for the single-byte code encoding `ch`.
///
/// # Arguments
///
/// * `ch` - Unicode scalar value.
/// * `table` - 256-entry SBCS BMP lookup table.
#[must_use]
fn lookup_sbcs_char(ch: char, table: &[u16; 256]) -> Option<u8> {
    let cp = u32::from(ch);
    if cp > 0xFFFF || cp == u32::from(UNMAPPED_SBCS) {
        return None;
    }
    let target = u16::try_from(cp).unwrap_or(UNMAPPED_SBCS);
    table
        .iter()
        .position(|&entry| entry == target)
        .and_then(|idx| u8::try_from(idx).ok())
}

/// Transcodes `src` bytes from `from` [`Codepage`] into `to` [`Codepage`].
///
/// # Arguments
///
/// * `src` - Input byte slice encoded in `from`.
/// * `from` - Source [`Codepage`].
/// * `to` - Destination [`Codepage`].
/// * `policy` - [`UnmappedCharPolicy`] governing decode/encode errors or replacement characters.
///
/// # Errors
///
/// Returns [`CoblangError::EncodingConversion`] if `policy` is [`UnmappedCharPolicy::StrictError`] and conversion fails.
pub fn transcode_bytes(
    src: &[u8],
    from: Codepage,
    to: Codepage,
    policy: UnmappedCharPolicy,
) -> Result<Vec<u8>> {
    let chars = decode_to_chars(src, from, policy)?;
    encode_from_chars(&chars, to, policy)
}

/// Implements ISO 2023 `FUNCTION NATIONAL-OF(src, replacement)` (`src_codepage` -> `PIC N` `UTF-16BE`).
///
/// # Arguments
///
/// * `src` - Source alphanumeric/UTF-8 byte slice.
/// * `src_codepage` - Source [`Codepage`].
/// * `policy` - [`UnmappedCharPolicy`] (`StrictError` or `Substitute(replacement)`).
///
/// # Errors
///
/// Returns [`CoblangError::EncodingConversion`] if decoding `src` fails under `StrictError`.
pub fn national_of(
    src: &[u8],
    src_codepage: Codepage,
    policy: UnmappedCharPolicy,
) -> Result<Vec<u8>> {
    transcode_bytes(src, src_codepage, Codepage::Utf16Be, policy)
}

/// Implements ISO 2023 `FUNCTION DISPLAY-OF(national_utf16be, replacement)` (`PIC N` `UTF-16BE` -> `dst_codepage`).
///
/// # Arguments
///
/// * `national_utf16be` - Source `USAGE NATIONAL` (`UTF-16BE`) byte slice.
/// * `dst_codepage` - Destination display [`Codepage`].
/// * `policy` - [`UnmappedCharPolicy`] (`StrictError` or `Substitute(replacement)`).
///
/// # Errors
///
/// Returns [`CoblangError::EncodingConversion`] if decoding or target encoding fails under `StrictError`.
pub fn display_of(
    national_utf16be: &[u8],
    dst_codepage: Codepage,
    policy: UnmappedCharPolicy,
) -> Result<Vec<u8>> {
    transcode_bytes(national_utf16be, Codepage::Utf16Be, dst_codepage, policy)
}

/// Pads or truncates `src_utf16be` to `dst_units` 16-bit `NATIONAL` character positions (`0x00, 0x20` space).
///
/// # Arguments
///
/// * `src_utf16be` - Source `UTF-16BE` byte slice (must have even byte length).
/// * `dst_units` - Target `PIC N(dst_units)` 16-bit character count.
/// * `justify` - [`NationalJustify::Left`] or [`NationalJustify::Right`].
///
/// # Errors
///
/// Returns [`CoblangError::EncodingConversion`] if `src_utf16be.len()` is odd.
pub fn move_national_utf16be(
    src_utf16be: &[u8],
    dst_units: usize,
    justify: NationalJustify,
) -> Result<Vec<u8>> {
    if !src_utf16be.len().is_multiple_of(2) {
        return Err(CoblangError::EncodingConversion {
            encoding: Codepage::Utf16Be.to_text_encoding(),
            byte_offset: src_utf16be.len().saturating_sub(1),
            message: String::from("NATIONAL UTF-16BE byte slice has odd length"),
        });
    }
    let src_units = src_utf16be.len() / 2;
    let dst_bytes = dst_units.saturating_mul(2);
    let mut out = Vec::with_capacity(dst_bytes);
    match justify {
        NationalJustify::Left => {
            let copy_bytes = src_units.min(dst_units).saturating_mul(2);
            out.extend_from_slice(src_utf16be.get(..copy_bytes).unwrap_or(&[]));
            for _ in src_units..dst_units {
                out.extend_from_slice(&[0x00, 0x20]);
            }
        }
        NationalJustify::Right => {
            if src_units >= dst_units {
                let skip_bytes = src_units.saturating_sub(dst_units).saturating_mul(2);
                out.extend_from_slice(src_utf16be.get(skip_bytes..).unwrap_or(&[]));
            } else {
                for _ in src_units..dst_units {
                    out.extend_from_slice(&[0x00, 0x20]);
                }
                out.extend_from_slice(src_utf16be);
            }
        }
    }
    Ok(out)
}

/// Truncates `src_utf8` cleanly on Unicode scalar boundaries to `<= max_chars` and `<= max_bytes`,
/// right-padding with ASCII `0x20` up to `pad_to_bytes`.
///
/// # Arguments
///
/// * `src_utf8` - Valid UTF-8 source string.
/// * `max_chars` - Maximum Unicode scalar count (`PIC U(max_chars)`).
/// * `max_bytes` - Maximum encoded UTF-8 byte budget (`BYTE-LENGTH` limit).
/// * `pad_to_bytes` - Fixed storage width to right-pad with `0x20` (`0` for unpadded dynamic slice).
#[must_use]
pub fn move_utf8_characters(
    src_utf8: &str,
    max_chars: usize,
    max_bytes: usize,
    pad_to_bytes: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(pad_to_bytes.max(src_utf8.len().min(max_bytes)));
    let mut buf = [0_u8; 4];
    for ch in src_utf8.chars().take(max_chars) {
        let enc = ch.encode_utf8(&mut buf).as_bytes();
        if out.len().saturating_add(enc.len()) > max_bytes {
            break;
        }
        out.extend_from_slice(enc);
    }
    while out.len() < pad_to_bytes {
        out.push(b' ');
    }
    out
}

/// Fills `dst` with repeating [`FigurativeUnit`] code units for `fig` in `codepage`.
///
/// # Arguments
///
/// * `dst` - Mutable destination storage slice.
/// * `codepage` - Target [`Codepage`].
/// * `fig` - COBOL [`FigurativeConstantChar`] to repeat.
///
/// # Errors
///
/// Returns [`CoblangError::EncodingConversion`] if `codepage` is `UTF-16` and `dst.len()` is odd.
pub fn fill_figurative_bytes(
    dst: &mut [u8],
    codepage: Codepage,
    fig: FigurativeConstantChar,
) -> Result<()> {
    match codepage.figurative_unit(fig) {
        FigurativeUnit::SingleByte(b) => {
            dst.fill(b);
            Ok(())
        }
        FigurativeUnit::DoubleByte(pair) => {
            let (chunks, rem) = dst.as_chunks_mut::<2>();
            if !rem.is_empty() {
                return Err(CoblangError::EncodingConversion {
                    encoding: codepage.to_text_encoding(),
                    byte_offset: dst.len().saturating_sub(1),
                    message: String::from(
                        "UTF-16 figurative destination buffer has odd byte length",
                    ),
                });
            }
            for slot in chunks {
                *slot = pair;
            }
            Ok(())
        }
    }
}

/// Compares two ASCII / Latin-1 alphanumeric slices under `ebcdic` collation with COBOL space padding.
///
/// # Arguments
///
/// * `lhs_ascii` - Left-hand alphanumeric operand bytes (ASCII / Latin-1).
/// * `rhs_ascii` - Right-hand alphanumeric operand bytes (ASCII / Latin-1).
/// * `ebcdic` - Active [`EbcdicCodepage`] collating sequence (`' '` < `'a'` < `'A'` < `'0'`).
#[must_use]
pub fn compare_alphanumeric_ebcdic(
    lhs_ascii: &[u8],
    rhs_ascii: &[u8],
    ebcdic: EbcdicCodepage,
) -> Ordering {
    let max_len = lhs_ascii.len().max(rhs_ascii.len());
    for idx in 0..max_len {
        let l_ascii = lhs_ascii.get(idx).copied().unwrap_or(b' ');
        let r_ascii = rhs_ascii.get(idx).copied().unwrap_or(b' ');
        let l_eb = ebcdic.ascii_to_ebcdic_byte(l_ascii);
        let r_eb = ebcdic.ascii_to_ebcdic_byte(r_ascii);
        let cmp = l_eb.cmp(&r_eb);
        if cmp != Ordering::Equal {
            return cmp;
        }
    }
    Ordering::Equal
}

/// Compares two `PIC N` (`USAGE NATIONAL`, `UTF-16BE`) slices with `U+0020` space right-padding.
///
/// # Arguments
///
/// * `lhs_utf16be` - Left-hand `UTF-16BE` byte slice (must have even byte length).
/// * `rhs_utf16be` - Right-hand `UTF-16BE` byte slice (must have even byte length).
///
/// # Errors
///
/// Returns [`CoblangError::EncodingConversion`] if either slice has odd byte length.
pub fn compare_national_utf16be(lhs_utf16be: &[u8], rhs_utf16be: &[u8]) -> Result<Ordering> {
    if !lhs_utf16be.len().is_multiple_of(2) {
        return Err(CoblangError::EncodingConversion {
            encoding: Codepage::Utf16Be.to_text_encoding(),
            byte_offset: lhs_utf16be.len().saturating_sub(1),
            message: String::from("LHS NATIONAL UTF-16BE byte slice has odd length"),
        });
    }
    if !rhs_utf16be.len().is_multiple_of(2) {
        return Err(CoblangError::EncodingConversion {
            encoding: Codepage::Utf16Be.to_text_encoding(),
            byte_offset: rhs_utf16be.len().saturating_sub(1),
            message: String::from("RHS NATIONAL UTF-16BE byte slice has odd length"),
        });
    }
    let (lhs_pairs, _) = lhs_utf16be.as_chunks::<2>();
    let (rhs_pairs, _) = rhs_utf16be.as_chunks::<2>();
    let max_units = lhs_pairs.len().max(rhs_pairs.len());
    for idx in 0..max_units {
        let l_unit = lhs_pairs
            .get(idx)
            .copied()
            .map_or(0x0020_u16, u16::from_be_bytes);
        let r_unit = rhs_pairs
            .get(idx)
            .copied()
            .map_or(0x0020_u16, u16::from_be_bytes);
        let cmp = l_unit.cmp(&r_unit);
        if cmp != Ordering::Equal {
            return Ok(cmp);
        }
    }
    Ok(Ordering::Equal)
}
