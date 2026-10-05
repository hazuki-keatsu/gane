# Motivation

## Why Build Another Go-like Compiler?

To be honest, Gane began with a practical motivation. I wanted to work on a project that would make me more competitive as an undergraduate majoring software engineering in China's highly competitive environment. After a long conversation with a close friend, I decided to study compilers and programming languages more seriously.

Before starting Gane, I had already built a small lexer and parser for C in Rust. It was not open-sourced, and I do not consider it a particularly good implementation.

Gane is not intended to replace Go or to become a complete implementation of the Go language. It is an experimental Go-like compiler project for exploring how a small compiler can be designed, tested, and evolved incrementally.

## Why Choose Go-like Syntax?

There are two main reasons.

First, I did not want to invent a language without an existing user model. A completely new language would make the project harder to understand and harder for others to use. Starting from a familiar language allows the project to focus on compiler construction rather than on designing every aspect of a new programming language at once.

Second, Go has a relatively small and approachable language surface, while still being used for real backend services and systems software. Go therefore provides a practical starting point for this experiment. At least, the parser and lexer can be more easy for being implemented.

## Technical Inspirations

One important inspiration is [Jeandle](https://github.com/jeandle/jeandle-jdk), a Java JIT runtime project open-sourced by Ant Group. Jeandle explores how LLVM can be integrated with the JVM by translating hot bytecode into LLVM IR and using LLVM to generate machine code. Its approach made me interested in the boundary between a language runtime and an LLVM-based compiler backend.

I wanted to explore related ideas with a smaller and more explicit language model. Gane currently focuses on an ahead-of-time compiler pipeline, but one long-term direction is to investigate whether a hybrid AOT/JIT architecture could improve selected long-running workloads. This is a research direction, not a claim about the current implementation or performance of Gane.

I later learned about **TangoLLVM**, a ByteDance project presented at the 2025 LLVM Developers' Meeting. TangoLLVM explores using LLVM as a backend for Go, including the challenges of Go's ABI, runtime metadata, and garbage collector. Although it is not currently an open-source implementation, its ideas were encouraging because they validated that using LLVM as part of a Go-oriented compiler architecture is a meaningful direction.

[TinyGo](https://github.com/tinygo-org/tinygo) is another important reference. TinyGo targets embedded and resource-constrained environments, and it shows how a Go-like language can be adapted to non-standard targets by making careful choices about the language, runtime, and generated code.

## Long-term Directions

The project currently has two broad long-term directions.

### Non-standard targets

The first direction is to explore compilation for non-standard environments, including bare-metal targets. A possible long-term milestone is compiling a Go-like port of [xv6](https://github.com/mit-pdos/xv6-public) with Gane and running it in a simulator or on suitable bare-metal hardware.

This goal will require a runtime, a target-specific ABI, memory-management support, and a linker or runtime integration layer. It is therefore a long-term milestone rather than a feature that the current compiler already supports.

### Hybrid AOT/JIT compilation

The second direction is to investigate a hybrid compilation architecture. The basic idea would be to use AOT compilation for startup and predictable code, then use runtime profiling and a lightweight JIT for selected hot code paths.

This direction may require:

- a runtime and profiling infrastructure
- a stable calling convention between generated code and the runtime
- code caching and replacement
- deoptimization or fallback behavior
- a memory-management strategy
- benchmarks that compare workloads under clearly defined conditions.

Whether this direction is practical for Gane remains an open question. It is better treated as an experiment than as a fixed promise.

## Project Lifetime

I cannot promise a fixed development schedule, but I expect to keep working on Gane for at least the next five years while I am in school. I do not expect to work on it full-time, and the project will evolve according to what I learn from implementation and experimentation.

Compiler projects often require years of incremental work before their designs and toolchains become mature. My goal is therefore not to implement every feature immediately, but to build a foundation that can be understood, validated, and extended over time.

These are the reasons behind Gane as I understand them today. The project may change as its implementation develops, and this document will be updated when its goals or assumptions change.

## About AI Assistance

At the very beginning of this project, Artificial Intelligence gave me a lot of helps. Since when I started my job, I had learnt a small range of knowledge about Concept of Compilation. So you may feel this project has a heavy taste of AI Slop. 

Don't worry. I will take control of the codebase gradually. Doing while Learning.
