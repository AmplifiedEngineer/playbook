---
name: engineering-standards-python
description: Python rules for code and reviews, covering style, idioms, types, errors, async, testing, packaging, security and the Ruff rules that check them. Use when writing or reviewing Python, or a diff that touches .py or .pyi files.
user-invocable: false
---

# Engineering Standards: Python

Language-specific engineering standards for Python, derived from the team's base standards. RFC 2119 keywords (MUST, SHOULD) carry their standard meanings.

**REQUIRED BACKGROUND:** engineering-standards owns the general rules (PR readiness, size, test types and requirements, the mocking philosophy, deployment). Read it first. This skill adds only the Python specifics and does not restate the base.

## How to use this skill

- The index below holds one line per rule. Each topic names a reference file in this skill's `references/` folder and says when to load it. Load only the files the work touches.
- When you review a diff that touches `.py` or `.pyi` files, load the reference for each topic the diff touches. Cite the rule and the Ruff rule, type checker flag or command that checks it.
- The project's own setup wins. When `pyproject.toml`, `ruff.toml`, the type checker config, CI or the contributing guide sets a rule, follow it over this skill.
- Set `lint.select` in the Ruff config and list each group or code you rely on. The default set depends on the version: up to Ruff 0.15 it is `E4`, `E7`, `E9` and `F`, and Ruff 0.16 grew it to about 400 rules. A rule the references mark (preview) also needs `preview = true`.
- Snippets target Python 3.12 and later unless a rule says otherwise.

## Rules index

### Style and naming

- Let the formatter own layout. Names follow PEP 8: `snake_case`, `CapWords`, `UPPER_CASE` constants, and a `_` prefix for internal names.
- Group imports (standard library, third party, local), import modules by absolute path, and no `import *`.
- Public modules, classes and functions have docstrings in one style per project.

Load `references/style-and-naming.md` when you name things, write imports, docstrings or comments, or review a diff for readability.

### Idioms

- Use comprehensions, generators, `enumerate`, `zip(strict=True)`, unpacking and f-strings over index loops and manual counters.
- Open every resource in a `with` block. Use `pathlib` for paths.
- Ask forgiveness for one narrow operation, and check first when the check is cheap and race free.

Load `references/idioms.md` when you write a loop, a comprehension, a `with` block, string formatting, path or datetime handling, a `match` or long `if` chain, a script entry point, or choose between `try` and a check first.

### Data modeling

- Model a record with a `dataclass` (frozen when you can), a closed set of values with an `Enum`, and an interface with a `Protocol`.
- Validate untrusted data at the boundary with Pydantic (or the project's validator), and pass plain typed objects inside.

Load `references/data-modeling.md` when you add a class that holds data, an enum, a protocol, a `TypedDict`, or code that parses input into objects.

### Types and typing

- Annotate every public function. Run mypy or pyright in strict mode in CI.
- No `Any` across a boundary, `X | None` over `Optional[X]`, and a runtime check over `cast`.

Load `references/types-and-typing.md` when you add or change annotations, a generic, a `cast`, a `# type: ignore`, or the type checker config.

### Error handling

- Catch the narrowest exception. No bare `except:`, and no `except` that passes in silence.
- Raise a custom exception from one project base class, and chain with `raise ... from err`.
- Log with `logger.exception` and lazy `%` arguments. No `print` in library or service code.

Load `references/error-handling.md` when you write `try`, `except`, `raise`, a custom exception, or a logging call.

### Functions and classes

- Small functions with one job. No mutable default arguments. Keyword-only parameters after the first one or two.
- A class has state and behavior, and holds collaborators as attributes. A module of functions replaces a class with only static methods.

Load `references/functions-and-classes.md` when you add a function, a class, a property, a dunder method or a decorator.

### Async and concurrency

- Do not block the event loop. Run related tasks in a `TaskGroup`, keep a reference to every task, put a timeout on each network call, and let cancellation through.
- Threads overlap waits, processes run CPU work in parallel. Use `concurrent.futures` executors in a `with` block.

Load `references/async-and-concurrency.md` when you write `async def`, create a task, use threads or processes, or add a lock, queue or semaphore.

### Performance

- Profile first. Pick the data structure for the access pattern, stream large data, and cache pure functions.

Load `references/performance.md` when a change targets speed or memory, or touches a hot loop.

### Testing

- pytest with strict markers, `importlib` import mode and warnings as errors. Parametrize cases, assert exceptions with `match`.
- Inject fakes before you patch. Patch where the name is looked up, with `autospec`. No sleeps and no real network.

Load `references/testing.md` when you write or review a test, a fixture or a mock, or reproduce a bug as a failing test.

### Packaging and tooling

- Metadata and tool config in `pyproject.toml`. uv for new projects, a committed lock file for applications, version ranges for libraries.
- Ruff for lint and format, a type checker and tests in CI, fast checks only in pre-commit, and supported CPython versions only.

Load `references/packaging-and-tooling.md` when you change `pyproject.toml`, dependencies, the lock file, CI, pre-commit or the supported Python versions.

### Security and input

- Bind SQL parameters, run processes with an argument list, and do not `eval`, unpickle or `yaml.load` untrusted data.
- Secrets from the environment, tokens from `secrets`, a timeout on every HTTP call, and `pip-audit` in CI.

Load `references/security-and-input.md` when code reads input from a network, a file or a user, runs a process, builds SQL, handles a path or a secret, or adds a dependency.

### Project structure

- src layout for packages, modules grouped by domain, one-way imports, and a thin entry point.

Load `references/project-structure.md` when you add a package or module, move code between modules, or add a CLI or config loading.

### Antipatterns

Load `references/antipatterns.md` when you review a Python diff. It is a checklist of the most common smells, each with a fix, and a check where one exists.

## Sources

The rule list started from the [Real Python best practices reference](https://realpython.com/ref/best-practices/) and the pages it links. Real Python content is copyrighted. The rules here are ideas restated in our own words. No text or code is copied from any source below, and every snippet is our own.

- Real Python entries used: [classes](https://realpython.com/ref/best-practices/classes/), [coding style](https://realpython.com/ref/best-practices/coding-style/), [code formatting](https://realpython.com/ref/best-practices/code-formatting/), [comments](https://realpython.com/ref/best-practices/comments/), [comprehensions](https://realpython.com/ref/best-practices/comprehensions/), [conditionals](https://realpython.com/ref/best-practices/conditionals/), [constants](https://realpython.com/ref/best-practices/constants/), [docstrings](https://realpython.com/ref/best-practices/docstrings/), [exception handling](https://realpython.com/ref/best-practices/exception-handling/), [public API surface](https://realpython.com/ref/best-practices/public-api-surface/), [Pythonic code](https://realpython.com/ref/best-practices/pythonic-code/), [type checking](https://realpython.com/ref/best-practices/type-checking/), [variables](https://realpython.com/ref/best-practices/variables/), [concurrency](https://realpython.com/ref/best-practices/concurrency/), [code testing](https://realpython.com/ref/best-practices/code-testing/), [dependency management](https://realpython.com/ref/best-practices/dependency-management/), [distribution](https://realpython.com/ref/best-practices/distribution/), [resource management](https://realpython.com/ref/best-practices/resource-management/), [security](https://realpython.com/ref/best-practices/security/), [virtual environments](https://realpython.com/ref/best-practices/virtual-environments/), [third-party libraries](https://realpython.com/ref/best-practices/third-party-libraries/), and the tutorial on [LBYL and EAFP](https://realpython.com/python-lbyl-vs-eafp/)
- [PEP 8](https://peps.python.org/pep-0008/), [PEP 20](https://peps.python.org/pep-0020/), [PEP 257](https://peps.python.org/pep-0257/) and [PEP 484](https://peps.python.org/pep-0484/) (public domain)
- [The Python documentation](https://docs.python.org/3/) (PSF License): the data model, `typing`, `dataclasses`, `enum`, the logging HOWTO and cookbook, `asyncio`, `concurrent.futures`, `unittest.mock` and the security notes of `subprocess`, `pickle` and `tarfile`
- [Typing best practices](https://typing.python.org/en/latest/reference/best_practices.html) from the Python typing project
- [mypy](https://mypy.readthedocs.io/) and [pyright](https://microsoft.github.io/pyright/) docs (MIT)
- [Pydantic](https://docs.pydantic.dev/) docs (MIT)
- [pytest](https://docs.pytest.org/) docs (MIT) and the [Hypothesis quickstart](https://hypothesis.readthedocs.io/) (MPL 2.0)
- [Python Packaging User Guide](https://packaging.python.org/) (CC BY-SA 3.0, ideas restated) and the [uv docs](https://docs.astral.sh/uv/) (MIT or Apache-2.0)
- [Python version status](https://devguide.python.org/versions/) (CC0) and [SPEC 0](https://scientific-python.org/specs/spec-0000/) from Scientific Python
- [OWASP Cheat Sheet Series](https://cheatsheetseries.owasp.org/) (CC BY-SA 4.0, ideas restated), [Bandit](https://bandit.readthedocs.io/) (Apache-2.0) and [pip-audit](https://github.com/pypa/pip-audit) (Apache-2.0)
- [Ruff rules](https://docs.astral.sh/ruff/rules/) (MIT). Rule codes and preview status were checked against Ruff 0.16.10 with `ruff rule --all`.
- [Google Python Style Guide](https://google.github.io/styleguide/pyguide.html) (CC BY 3.0)
- [The Hitchhiker's Guide to Python](https://docs.python-guide.org/) (CC BY-NC-SA 3.0, ideas only)
- [Modern Good Practices for Python Development](https://www.stuartellis.name/articles/python-modern-practices/) by Stuart Ellis (CC BY-NC-SA 4.0, ideas only)
