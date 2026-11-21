class Widget
end

class Shop
  def self.build
    w = Widget.new
    def w.extra
      1
    end
    w
  end
end

Widget.extra
Widget.new.extra
