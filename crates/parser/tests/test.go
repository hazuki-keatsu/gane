// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 2009 The Go Authors.
// SPDX-FileCopyrightText: 2026 Hazuki Keatsu
//
// Adapted from the Go standard library for Gane.

//go:build x86

// test.go for front-end test
package gane

import (
	"fmt"
	"math"
	"strings"
	"sync"
	"unsafe"
)

// -----------------------------------------------------------------------------
// 1. Package declarations: constants, variables, aliases, and defined types
// -----------------------------------------------------------------------------

const (
	DefaultName            = "Gane"
	Answer                 = 6 * 7
	RuneA                  = 'a'
	ComplexConstant        = 2 + 3i
	UntypedFloat           = 1.25
	TypedUint16     uint16 = 16
)

// iota, shifts, and boolean and string constant expressions.
const (
	Read = 1 << iota
	Write
	Execute
	AllPermissions = Read | Write | Execute
	IsAnswer       = Answer == 42
	Greeting       = "hello, " + DefaultName
)

var (
	packageCounter int = 3
	implicitValue      = math.Pi
	zeroValue      int
	sharedState    = struct {
		sync.Mutex
		values []int
	}{}
)

type UserID int64
type Text = string // An alias is distinct from defining a new type.

// -----------------------------------------------------------------------------
// 2. Aggregate types, embedded fields, tags, pointers, and methods
// -----------------------------------------------------------------------------

type Person struct {
	Name string `json:"name"`
	age  int
}

func (p Person) GetName() string { return p.Name }

func (p *Person) SetAge(newAge int) {
	if newAge >= 0 {
		p.age = newAge
	}
}

type Employee struct {
	Person  // Embedded fields promote Person's methods.
	Company string
}

type Point struct{ X, Y float64 }

func (p Point) Add(other Point) Point { return Point{p.X + other.X, p.Y + other.Y} }

type NamedArray [3]int
type IntSlice []int
type ScoreMap map[string]int
type SendOnly chan<- int
type ReceiveOnly <-chan int

// -----------------------------------------------------------------------------
// 3. Interfaces, method sets, type assertions, and type switches
// -----------------------------------------------------------------------------

type Shape interface {
	Area() float64
}

type Stringer interface {
	String() string
}

type Describer interface {
	Shape
	Stringer
}

type Rectangle struct{ Width, Height float64 }

func (r Rectangle) Area() float64  { return r.Width * r.Height }
func (r Rectangle) String() string { return fmt.Sprintf("rectangle(%g,%g)", r.Width, r.Height) }

type Circle struct{ Radius float64 }

func (c Circle) Area() float64  { return math.Pi * c.Radius * c.Radius }
func (c Circle) String() string { return fmt.Sprintf("circle(%g)", c.Radius) }

func printArea(shape Shape) { fmt.Printf("%.2f\n", shape.Area()) }

func inspect(value any) string {
	switch v := value.(type) {
	case nil:
		return "nil"
	case string:
		return strings.ToUpper(v)
	case fmt.Stringer:
		return v.String()
	default:
		return fmt.Sprintf("%T", v)
	}
}

func assertShape(value any) (Shape, bool) {
	shape, ok := value.(Shape)
	return shape, ok
}

// -----------------------------------------------------------------------------
// 4. Functions: multiple and named results, variadics, closures, recursion, and defer
// -----------------------------------------------------------------------------

func add(a, b int) int { return a + b }

func divide(dividend, divisor int) (quotient, remainder int, err error) {
	if divisor == 0 {
		return 0, 0, fmt.Errorf("division by zero")
	}
	return dividend / divisor, dividend % divisor, nil
}

func sum(values ...int) int {
	total := 0
	for _, value := range values {
		total += value
	}
	return total
}

func makeAdder(base int) func(int) int {
	return func(value int) int { return base + value } // A closure that captures base.
}

func fibonacci(n int) int {
	if n < 2 {
		return n
	}
	return fibonacci(n-1) + fibonacci(n-2)
}

func deferredResult() (result string) {
	defer func() { result += "!" }()
	defer func(text string) { result = strings.ToUpper(text) }("done")
	return "ignored" // A defer can modify a named return value.
}

func recoverFromPanic() (recovered any) {
	defer func() { recovered = recover() }()
	panic("example panic")
}

// -----------------------------------------------------------------------------
// 5. Arrays, slices, maps, strings, composite literals, and built-ins
// -----------------------------------------------------------------------------

func collections() (NamedArray, IntSlice, ScoreMap, string) {
	array := NamedArray{1, 2, 3}
	slice := []int{1, 2, 3}
	slice = append(slice[:1], 9, 8)
	copyOfSlice := make([]int, len(slice), cap(slice)+2)
	copy(copyOfSlice, slice)

	scores := map[string]int{"alice": 10, "bob": 20}
	scores["carol"] = len(copyOfSlice)
	delete(scores, "bob")
	_, exists := scores["alice"]

	bytes := []byte("Go")
	text := string(append(bytes, '!'))
	return array, copyOfSlice, scores, fmt.Sprintf("%s:%t", text, exists)
}

func literalsAndIndexes() int {
	matrix := [2][2]int{{1, 2}, {3, 4}}
	pointer := &matrix[1][0]
	value := *pointer

	// Composite literals for slices, maps, and structs.
	_ = []Point{{X: 1, Y: 2}, {3, 4}}
	_ = map[UserID]Text{1: "one"}
	return value + int(unsafe.Sizeof(matrix))
}

// -----------------------------------------------------------------------------
// 6. Operators, conversions, if/switch, and the three for forms
// -----------------------------------------------------------------------------

func controlFlow(input int) (result int) {
	value := float64(input)
	negated := -input
	bitwise := ^input
	logical := input > 0 && input != 1 || input == 42

	if init := int(value); init > 10 {
		result += init
	} else if logical {
		result++
	} else {
		result = negated + bitwise
	}

	for index := 0; index < 3; index++ {
		if index == 1 {
			continue
		}
		result += index
	}

	for result < 10 {
		result += 2
	}

	for {
		result--
		break
	}

	switch remainder := result % 3; remainder {
	case 0:
		result += 100
	case 1, 2:
		result += 10
	default:
		panic("unreachable")
	}

	switch {
	case result < 0:
		result = -result
	case result == 0:
		fallthrough
	default:
		result++
	}
	return result
}

func rangeForms() int {
	total := 0
	for index, value := range []int{2, 4, 6} {
		total += index + value
	}
	for _, runeValue := range "Go" {
		total += int(runeValue)
	}
	for key, value := range map[string]int{"x": 1} {
		total += len(key) + value
	}
	return total
}

// -----------------------------------------------------------------------------
// 7. Labels, goto, concurrency primitives, select, and anonymous goroutines
// -----------------------------------------------------------------------------

func labelsAndGoto(limit int) int {
	sum := 0
Outer:
	for row := 0; row < limit; row++ {
		for column := 0; column < limit; column++ {
			if row+column > limit {
				continue Outer
			}
			sum += row + column
		}
	}
	if sum == 0 {
		goto Empty
	}
	return sum
Empty:
	return -1
}

func channelAndSelect() int {
	channel := make(chan int, 1)
	var send SendOnly = channel
	var receive ReceiveOnly = channel
	send <- 7

	select {
	case value := <-receive:
		close(channel)
		return value
	default:
		return 0
	}
}

func startGoroutine(done chan<- struct{}) {
	go func() { done <- struct{}{} }()
}

// -----------------------------------------------------------------------------
// 8. Generics: type parameters, constraints, approximate type sets, and instantiation
// -----------------------------------------------------------------------------

type Number interface {
	~int | ~int64 | ~float64
}

type Pair[T any] struct {
	First, Second T
}

func (p Pair[T]) Swap() Pair[T] { return Pair[T]{p.Second, p.First} }

func max[T Number](left, right T) T {
	if left > right {
		return left
	}
	return right
}

func genericExamples() (Pair[string], int) {
	pair := Pair[string]{First: "left", Second: "right"}
	return pair.Swap(), max(3, 5) // Type arguments can be inferred from arguments.
}

// -----------------------------------------------------------------------------
// 9. Program entry point: run a few examples and retain the rest for front-end coverage.
// -----------------------------------------------------------------------------

func main() {
	employee := Employee{Person: Person{Name: "Bob"}, Company: "Google"}
	employee.SetAge(30)
	printArea(Rectangle{Width: 3, Height: 4})
	fmt.Println(employee.GetName(), inspect(Circle{Radius: 5}))
	fmt.Println(add(Answer, packageCounter), sum(1, 2, 3), deferredResult())
}
