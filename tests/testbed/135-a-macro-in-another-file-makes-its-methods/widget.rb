class Widget
  include Macros
  add_helper :color
  add_flags :active, :hidden

  def run_color
  end

  def use
    color_helper
    hidden?
  end
end

class Gadget
  include Macros
  add_helper some_name
end
