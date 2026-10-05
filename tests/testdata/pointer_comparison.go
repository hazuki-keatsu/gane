package main

func assert_true(ok bool) {
	if !ok {
		var a [1]int
		a[1] = 1
	}
}

func assert_false(nok bool) {
	if nok {
		var a [1]int
		a[1] = 1
	}
}

func main() {
	// The same address
	var same int = 1
	var p1 *int = &same
	var p2 *int = &same

	assert_true(p1 == p2)

	// The difference address
	var diff_object int = 1
	var p3 *int = &diff_object

	assert_false(p1 == p3)
}