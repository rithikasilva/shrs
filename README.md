
<div align="center">

# sh.rs

rust library to build your own shell

</div>

## TOOD

- [x] pipes
- [x] file redirection
- [ ] configuration scheme (config file? builder pattern?)
- [ ] better logging + error reporting (different ways of displaying exit status)
- [x] background process + job control (&)
- [ ] subshells
- [ ] control flow
- [ ] signals (^C, ^\, ^Z etc)
- [ ] completion
- [ ] history
- [ ] alias
- [ ] test suite to ensure posix compliant

## RESOURCES

- [build your own shell](https://github.com/tokenrove/build-your-own-shell)
- [grammar for posix shell](https://pubs.opengroup.org/onlinepubs/9699919799/utilities/V3_chap02.html#tag_18_10)
- [oursh: rust shell using lalrpop](https://github.com/nixpulvis/oursh)
- [gnu: implementing a job control shell](https://www.gnu.org/software/libc/manual/html_node/Implementing-a-Shell.html)
- [A Brief Introduction to termios](https://blog.nelhage.com/2009/12/a-brief-introduction-to-termios/)