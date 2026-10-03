module Shop
  class Widget
  end
end

class Job
  def run
    Shop::Widget.new
  end
end
