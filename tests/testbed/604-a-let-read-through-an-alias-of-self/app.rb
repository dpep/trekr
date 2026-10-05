class Widget
  def build
    this = self
    Class.new do
      define_method(:part) { this.part_name }
    end
  end

  def part_name
    :knob
  end
end
