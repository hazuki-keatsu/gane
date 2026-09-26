package main

type Person struct {
	age int
	weight int
}

func main() {
	var p Person
	p.age = 12
	p.weight = 75

	var a [10]int
	a[5] = 12
	a[9] = 75

	var q [2]Person
	q[0].age = 12
	q[0].weight = 75
	q[1].age = 11
	q[1].weight = 60
}