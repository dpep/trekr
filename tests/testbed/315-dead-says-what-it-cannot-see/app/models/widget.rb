class Widget
  def display_title
    "title"
  end

  def marshal_dump
    [display_title]
  end

  def marshal_load(data)
    data
  end

  def to_partial_path
    "widgets/widget"
  end

  def each
    yield self
  end

  def lonely
    :lonely
  end

  def quiet
    :quiet
  end
end
