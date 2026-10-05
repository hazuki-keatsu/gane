package main

import (
	"bufio"
	"fmt"
	"os"
	"runtime"
	"unicode"
)

func classificationMask(r rune) uint8 {
	var mask uint8
	if unicode.IsLetter(r) {
		mask |= 1
	}
	if unicode.IsDigit(r) {
		mask |= 2
	}
	if unicode.IsPrint(r) {
		mask |= 4
	}
	return mask
}

func main() {
	writer := bufio.NewWriter(os.Stdout)
	defer writer.Flush()

	fmt.Fprintf(writer, "GANE_UNICODE_DIFF_V1\t%s\t%s\n", runtime.Version(), unicode.Version)

	var start, previous rune = -1, -1
	var previousMask uint8
	flush := func() {
		if start >= 0 {
			fmt.Fprintf(writer, "%X\t%X\t%X\n", start, previous, previousMask)
		}
	}

	for r := rune(0); r <= unicode.MaxRune; r++ {
		// Go strings and Gane's UTF-8 decoder cannot represent surrogate code points.
		if 0xD800 <= r && r <= 0xDFFF {
			continue
		}
		mask := classificationMask(r)
		if mask == 0 {
			flush()
			start, previous = -1, -1
			previousMask = 0
			continue
		}
		if start >= 0 && r == previous+1 && mask == previousMask {
			previous = r
			continue
		}
		flush()
		start, previous, previousMask = r, r, mask
	}
	flush()
}
