class Gadget
  def name_for(x)
    m = "build_#{x}"
    m
  end

  def run(m)
    send(m)
  end

  def build_box
    :box
  end
end

class Crate
  def run(x)
    m = "make_#{x}"
    m = "default"
    send(m)
  end

  def make_lid
    :lid
  end
end

class Shelf
  def run(x)
    m = "stack_#{x}"
    m = "default" if x.nil?
    send(m)
  end

  def stack_up
    :up
  end
end
