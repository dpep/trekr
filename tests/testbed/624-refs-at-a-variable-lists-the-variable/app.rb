class Widget
  def go(key)
    key
  end

  def run(key)
    @item = { size: 1 }
    @item.fetch(:size) + go(key)
  end
end
Widget.new.run(1)
