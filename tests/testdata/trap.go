package main

func stack1() {
	var slot1 int = 1
	stack2()
}

func stack2() {
	var slot2 int = 2

	// trap
	var a [1]int
	a[1] = 1
}

func main() {
	stack1()
}