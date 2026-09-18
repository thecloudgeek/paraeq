# AutoEq results index

This file is a hand-written ~20-line excerpt modeled on AutoEq's
`results/INDEX.md`. It is TEST DATA, not a golden fixture: `fixtures/` stays
oracle-generated. Like the real index, the link hrefs are percent-ENCODED
(spaces as `%20`, `#` as `%23`, `+` as `%2B`, `&` as `%26`); parsing must
DECODE them to the literal path (so the fetch layer encodes exactly once). It
deliberately includes the SAME model measured under two different sources/rigs
(Sennheiser HD 650), so parsing must preserve duplicates as distinct entries,
plus hrefs with encoded spaces and `#`/`+`/`&`, and non-matching lines (this
heading, prose, blanks) that must be skipped.

## Over-ear

- [Sennheiser HD 650](./oratory1990/harman_over-ear_2018/Sennheiser%20HD%20650) by oratory1990 on harman_over-ear_2018
- [Sennheiser HD 650](./crinacle/GRAS%2043AG-7/Sennheiser%20HD%20650) by crinacle on GRAS 43AG-7
- [Sennheiser HD 800 S](./oratory1990/harman_over-ear_2018/Sennheiser%20HD%20800%20S) by oratory1990 on harman_over-ear_2018
- [AKG K371](./oratory1990/harman_over-ear_2018/AKG%20K371) by oratory1990 on harman_over-ear_2018
- [Beyerdynamic DT 1990 Pro](./oratory1990/harman_over-ear_2018/Beyerdynamic%20DT%201990%20Pro) by oratory1990 on harman_over-ear_2018

## In-ear

- [Moondrop Blessing 2](./crinacle/711/Moondrop%20Blessing%202) by crinacle on 711
- [Shozy & Neo BG](./crinacle/711/Shozy%20%26%20Neo%20BG) by crinacle on 711
- [Test Model #1+2](./oratory1990/harman_in-ear_2019/Test%20Model%20%231%2B2) by oratory1990 on harman_in-ear_2019

Some trailing prose that is not an entry and must be skipped.
