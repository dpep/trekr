class Base
  def run
  end

  def ping
  end
end

class Widget < Base
  def twice
  end

  def once
  end

  def many
  end

  def live
  end

  def lone
  end

  def run
  end

  def ping
  end

  def each
  end

  def use
    live
    lone
  end

  private

  def hidden
  end
end

class Other
  def lone
  end

  def ping
  end
end
