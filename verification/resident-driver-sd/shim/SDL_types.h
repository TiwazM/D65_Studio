/* Minimal stand-in for SDL2's integer typedefs; the harness needs no SDL. */
#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
typedef uint8_t Uint8; typedef int8_t Sint8; typedef uint16_t Uint16; typedef int16_t Sint16;
typedef uint32_t Uint32; typedef int32_t Sint32; typedef uint64_t Uint64; typedef int64_t Sint64;
typedef int SDL_bool;
