#let data-path = sys.inputs.at("data", default: "")
#assert(data-path != "", message: "Pass generated JSON with --input data=<path>")

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
#let section-fill = luma(235)
#let section-stroke = 0.45pt + black
#let sheet-columns = 3
#let sheet-width = 297mm - 14mm
#let sheet-height = 210mm - 14mm
#let sheet-label-height = 16pt
#let unit-gap = -0.5pt
#let measured-height-safety = 1.12

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
        block(height: answer-slot-height)[
          #text(fill: answer-color, size: answer-text-size)[#str(answer)]
        ]
      } else {
        [#v(answer-slot-height)]
      }),
    )
  }
}

#let question-unit(question, show-answers: true) = block(width: 100%, breakable: false)[
  #table(
    columns: (2.05fr, 1.2fr),
    stroke: cell-stroke,
    inset: 3pt,
    align: horizon + left,
    question-cell(question),
    answer-cell(question.at("answers", default: ()), show-answers: show-answers),
  )
]

#let section-unit(title) = block(width: 100%, breakable: false)[
  #rect(
    width: 100%,
    fill: section-fill,
    stroke: section-stroke,
    inset: (x: 3pt, y: 2pt),
  )[
    #text(weight: "bold", size: 8.8pt)[#title]
  ]
]

#let build-entries(questions) = {
  let entries = ()
  let current-section = none
  let section-number = 0
  for question in questions {
    let section = question.at("section", default: "")
    let label = ""
    if section != "" and section != current-section {
      section-number += 1
      label = str(section-number) + ". " + section
      current-section = section
    }
    entries.push((question, label))
  }
  entries
}

#let column-length(page-columns, column) = {
  if column == 0 {
    page-columns.at(0).len()
  } else if column == 1 {
    page-columns.at(1).len()
  } else {
    page-columns.at(2).len()
  }
}

#let entry-unit(question, section, show-answers: true) = block(width: 100%, breakable: false)[
  #if section != "" {
    section-unit(section)
    v(unit-gap)
  }
  #question-unit(question, show-answers: show-answers)
]

#let measured-entry-height(entry, column-width) = {
  let question = entry.at(0)
  let section = entry.at(1)
  let content = entry-unit(question, section, show-answers: true)
  measure(block(width: column-width)[#content]).height * measured-height-safety
}

#let pack-pages(entries, column-width, column-height) = {
  let pages = ()
  let page-columns = ((), (), ())
  let column-heights = (0pt, 0pt, 0pt)
  let column = 0

  for (index, entry) in entries.enumerate() {
    let height = measured-entry-height(entry, column-width)
    let gap = if column-length(page-columns, column) == 0 { 0pt } else { unit-gap }

    if column-heights.at(column) + gap + height > column-height and column < sheet-columns - 1 {
      column += 1
      gap = 0pt
    }

    if column-heights.at(column) + gap + height > column-height and column == sheet-columns - 1 and column-length(page-columns, column) > 0 {
      pages.push(page-columns)
      page-columns = ((), (), ())
      column-heights = (0pt, 0pt, 0pt)
      column = 0
      gap = 0pt
    }

    if column == 0 {
      page-columns.at(0).push(entry)
    } else if column == 1 {
      page-columns.at(1).push(entry)
    } else {
      page-columns.at(2).push(entry)
    }
    column-heights.at(column) = column-heights.at(column) + gap + height
  }

  if page-columns.at(0).len() + page-columns.at(1).len() + page-columns.at(2).len() > 0 {
    pages.push(page-columns)
  }
  pages
}

#let render-column(items, show-answers: true) = {
  for (index, entry) in items.enumerate() {
    if index > 0 {
      v(unit-gap)
    }
    entry-unit(entry.at(0), entry.at(1), show-answers: show-answers)
  }
}

#let render-sheet-page(page-columns, label, show-answers: true) = {
  block(height: sheet-label-height)[
    #text(weight: "bold")[#label]
  ]

  grid(
    columns: (1fr, 1fr, 1fr),
    gutter: 0pt,
    ..page-columns.map(column => [
      #render-column(column, show-answers: show-answers)
    ]),
  )
}

#context {
  let column-width = sheet-width / sheet-columns
  let column-height = sheet-height - sheet-label-height
  let entries = build-entries(questions)
  let pages = pack-pages(entries, column-width, column-height)

  if pages.len() == 0 {
    [#text(weight: "bold")[問題がありません]]
  } else {
    for (index, page-columns) in pages.enumerate() {
      if index > 0 {
        pagebreak()
      }
      render-sheet-page(page-columns, "解答 " + str(index + 1), show-answers: true)
      pagebreak()
      render-sheet-page(page-columns, "問題 " + str(index + 1), show-answers: false)
    }
  }
}
