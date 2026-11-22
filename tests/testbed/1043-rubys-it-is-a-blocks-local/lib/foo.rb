class Foo
  def run(items)
    items.map { _1 + 1 }
    items.map { it + 1 }
    items.each { it.to_s; it }
  end
end
