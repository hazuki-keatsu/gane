package main

const Con int = 12

func check(ok bool) {
	if !ok {
		var a [1]int
		a[1] = 0
	}
}

func main() {
	var spe int = Con + 13

	check(spe == 25)
}
