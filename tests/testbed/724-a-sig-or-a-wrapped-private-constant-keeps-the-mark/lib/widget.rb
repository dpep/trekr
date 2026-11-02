class Widget
  # @api private
  sig { void }
  def reset
  end

  # Internal: rebuilds the cache.
  sig do
    params(force: T::Boolean).void
  end
  def rebuild(force)
  end

  # @api private
  def flush
  end

  sig { void }
  def expand
  end

  class Store
    def put
    end
  end
  private_constant(
    :Store
  )
end
