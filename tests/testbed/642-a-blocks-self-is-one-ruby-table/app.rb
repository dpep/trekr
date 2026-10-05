class Widget
  def label
    "w"
  end

  def run
    instance_eval do
      shine
    end
    try(:label)
    send(:label)
  end
end

class Gadget
  def label
    "g"
  end

  def shine
    "g"
  end
end

class Host
  register(Data.define(:a) do
    define_method(:extra) { a }
  end)

  def go
    extra
  end
end
