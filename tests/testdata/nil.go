// IR lowering failed: unsupported lowering construct: type
// IR lowering will reject nil type.
package main

var globalNil *int = nil

func isNil(pointer *int) bool {
	// Sema currently accepts this comparison, but IR lowering does not yet
	// materialize the nil operand as an IR null constant.
	return pointer == nil
}

func main() {
	// These calls exercise p == nil with both null and non-null pointers.
	var localZero *int
	_ = isNil(globalNil)
	_ = isNil(localZero)

	var value int = 42
	var pointer *int = &value
	_ = isNil(pointer)
}
