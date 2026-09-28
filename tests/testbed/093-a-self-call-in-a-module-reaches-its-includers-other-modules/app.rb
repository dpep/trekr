module Rendering
  def render
    options
  end

  def draw
    palette
  end
end

module Options
  def options
  end
end

module Palette
  def palette
  end
end

class Page
  include Rendering
  include Options
end
