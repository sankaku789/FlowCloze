#let data-path = sys.inputs.at("data", default: "")
#assert(data-path != "", message: "Pass generated JSON with --input data=<path>")

// TODO: 添付イメージに近づけるため，複数qblockの段組み調整，
//       問題文セルの高さ制御，解答欄の余白を改善する。

#let data = json(data-path)
#let questions = data.at("questions", default: ())

#set page(paper: "a4", flipped: true, margin: 7mm)
#set text(font: ("Noto Sans CJK JP", "Droid Sans Fallback", "DejaVu Sans"), size: 8.4pt)
#set par(leading: 0.58em, justify: false)

#let answer-color = rgb("#e11d1d")
#let answer-text-size = 9.4pt
#let answer-slot-height = 16pt
#let cell-stroke = 0.45pt + black
#let slot-stroke = 0.35pt + black

#let write-lines(body) = {
  for line in str(body).split("\n") {
    [#line]
    linebreak()
  }
}

#let question-cell(question) = {
  [
    #write-lines(question.at("question", default: ""))
  ]
}

#let answer-cell(answers, show-answers: true) = {
  if answers.len() == 0 {
    table(
      columns: (1fr,),
      stroke: slot-stroke,
      inset: 3pt,
      [#v(answer-slot-height)],
    )
  } else {
    table(
      columns: (1fr,),
      stroke: slot-stroke,
      inset: 3pt,
      ..answers.map(answer => if show-answers {
        [#text(fill: answer-color, size: answer-text-size)[#str(answer)]]
      } else {
        [#v(answer-slot-height)]
      }),
    )
  }
}

#let sheet(label, show-answers: true) = {
  heading(level: 2, label)

  let cells = ()
  for chunk in questions.chunks(3) {
    for question in chunk {
      cells.push(question-cell(question))
      cells.push(answer-cell(question.at("answers", default: ()), show-answers: show-answers))
    }

    for _ in range(3 - chunk.len()) {
      cells.push([])
      cells.push([])
    }
  }

  table(
    columns: (2.05fr, 1.2fr, 2.05fr, 1.2fr, 2.05fr, 1.2fr),
    stroke: cell-stroke,
    inset: 3pt,
    align: horizon + left,
    ..cells,
  )
}

#sheet("解答入り", show-answers: true)
#pagebreak()
#sheet("演習用", show-answers: false)
