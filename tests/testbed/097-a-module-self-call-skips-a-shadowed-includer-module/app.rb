module Formatting
  def label
    "b"
  end
end

module Labeled
  def label
    "a"
  end

  def show
    label
    stamp
  end
end

class Widget
  include Formatting
  include Labeled

  def stamp
  end
end

module Other
  def tag; end
  def show2; tag; end
end

module Tagging
  def tag; end
end

class Gadget
  include Tagging
  include Other
end

Widget.new.show
Gadget.new.show2
