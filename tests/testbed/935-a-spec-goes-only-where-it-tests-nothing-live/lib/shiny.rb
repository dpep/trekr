class Shiny
  def visible?
    true
  end

  def has_gloss?
    true
  end

  def name
    "w"
  end

  def kind
    :k
  end

  def label
    "l"
  end
end

class Equal
  def label
    "l"
  end

  def ==(other)
    other.is_a?(Equal)
  end
end

shiny = Shiny.new
shiny.visible?
shiny.has_gloss?
shiny.name
shiny.kind
Equal.new == Equal.new
