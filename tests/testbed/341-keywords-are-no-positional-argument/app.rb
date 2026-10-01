class Widget
  def refresh(name: nil, value: nil); end

  def resize(width, height:); end

  def label(text); end
end

class Shop
  def run(thing)
    Widget.new.refresh(name: "a", value: 1)
    Widget.new.resize(3, height: 4)
    Widget.new.label(text: "a")
    thing.refresh(name: "b")
    thing.resize(3, height: 4)
    thing.label(text: "a")
  end
end
