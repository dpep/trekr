class Widget
  class AbortedError < StandardError
  end

  LIMIT = 3

  def expand
    render_parts
  end

  def reset # :nodoc:
  end

  # Drops what is buffered.
  #
  # @api private
  def flush
  end

  # Internal: rebuilds the cache.
  def rebuild
  end

  def _compact
  end

  private

  def render_parts
    "parts"
  end
end

class Widget::Cache # :nodoc:
  def fetch
  end
end

class Widget
  class Store
    def put
    end
  end
  private_constant :Store
end
