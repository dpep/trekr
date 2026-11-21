class Report
  def build
    header
  end

  def header
    :h
  end

  def assemble
    footer
  end

  def footer
  end
end

class FancyReport < Report
  def header
    super
  end
end
