require "widget"

def helper
end

helper

class Widget
  def require(name)
  end

  def run
    helper
  end
end

[1].each do
  helper
end
