class Widget
  attr_reader :unused_reader
  attr_writer :unused_writer
  attr_accessor :half

  def initialize
    self.half = 1
  end

  def lonely
  end
end
Widget.new
