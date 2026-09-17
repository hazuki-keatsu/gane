package main

const Limit int = 4

var initial int = 1
var globalFlag bool = true
var globalByte byte

type Point struct {
	x int
	y int
}

type Node struct {
	value int
	next  *Node
}

// if not ok, lli will trap
func check(ok bool) {
	if !ok {
		var failure [1]int
		failure[1] = 0
	}
}

func add(left int, right int) int {
	return left + right
}

func identity(value int) int {
	return value
}

func bumpGlobal() {
	initial++
}

func main() {
	// Globals, constants, scalar local declarations, calls, return values,
	// assignment, and all supported scalar arithmetic/comparisons.
	var value int = add(initial, Limit)
	value++
	value--
	value = value * 2
	value = value / 5
	value = value % 3
	check(value == 2)
	check(value != 3)
	check(value < 3)
	check(value <= 2)
	check(value > 1)
	check(value >= 2)
	check(-value == -2)
	check(+value == 2)
	check((value + 1) == 3)
	check(globalFlag)
	var byteValue byte
	byteValue++
	check(byteValue > globalByte)
	bumpGlobal()
	check(initial == 2)

	// Blocks, if/else, and short-circuit expressions. The right sides would
	// trap if evaluated, so these also test that the CFG short-circuits.
	{
		var condition bool = value == 2
		if condition {
			value = identity(value + 1)
		} else {
			value = 0
		}
	}
	if value == 3 {
		value = value - 1
	}
	var values [4]int
	var skipAnd bool = false
	var skipOr bool = true
	check(!(skipAnd && values[Limit] == 0))
	check(skipOr || values[Limit] == 0)

	// Arrays, integer and byte indexes, conditional loops, continue, break,
	// and an infinite loop with an explicit exit.
	values[0] = value
	var index int = 1
	for index < Limit {
		if index == 2 {
			index++
			continue
		}
		values[index] = values[index - 1] + index
		index++
	}
	var byteIndex byte
	byteIndex++
	byteIndex++
	values[byteIndex] = values[0]
	var once int
	for {
		once++
		break
	}
	check(once == 1)
	check(values[0] == 2)
	check(values[1] == 3)
	check(values[2] == 2)
	check(values[3] == 3)

	// Struct fields, address-of, dereference, indirect field selection, and
	// zero/copy aggregate assignments.
	var point Point = Point{}
	point.x = values[3]
	point.y = add(point.x, Limit)
	var pointPtr = &point
	pointPtr.x++
	var pointCopy Point = point
	var arrayCopy [4]int = values
	var scalarPtr = &value
	*scalarPtr = pointCopy.x - 1
	check(value == 3)
	check(pointCopy.x == 4)
	check(pointCopy.y == 7)
	check(arrayCopy[3] == 3)

	// Recursive pointer fields and a void call used as an expression statement.
	var node Node
	node.value = pointCopy.y
	node.next = &node
	if node.next.value > 0 {
		node.next.value--
	} else {
		node.value = 0
	}
	check(node.value == 6)
}
