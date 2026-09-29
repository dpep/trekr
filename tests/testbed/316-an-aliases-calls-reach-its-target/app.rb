class Column
  def array
    true
  end
  alias :array? :array

  def plain
    false
  end
  alias :plain? :plain
end

class Table
  def arrays(column)
    Column.new.array? && column.array?
  end

  def first
    Column.new.array?
  end
end
