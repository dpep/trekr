module Shop
  class Widget
    SIZES = %i[small large].freeze
    def go
      SIZES.first
      Widget.new
    end
  end
end

module Shop
  class Other
    def x = Widget::SIZES
  end
end

Shop::Widget.new
Shop::Widget::SIZES
class Widget; end
Widget.new
