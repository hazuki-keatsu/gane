package main

func add(x int, y int) int {
	return x + y
}

func main() {
	var a int = 1
	var b int = 2
	var c int = add(a, b)
	
	// IR lowering failed: unsupported lowering construct: value-returning expression statement
	// add(a, b)
	_ = add(a, b)
}